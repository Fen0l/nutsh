//! The Prism Central client. Every request goes through `send`, which adds Basic auth and
//! retries a 429 once. Mutations go through `act`, which enforces the ETag rule.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{StreamExt, TryStreamExt, stream};
use nutsh_catalog::{
    ActionReturn, KINDS, Kind, Method, NAMESPACES, Namespace, RateLimit, fill_placeholders,
    placeholder_count, version_in, with_version,
};
use reqwest::header::{self, HeaderMap};
use reqwest::{Method as HttpMethod, StatusCode, Url};
use serde_json::Value;

use crate::bucket::{self, Bucket};
use crate::entity::Entity;
use crate::envelope;
use crate::error::certificate_error;
use crate::error::{PrismError, describe};
use crate::limits::{Advertised, MAX_RETRY_AFTER};
use crate::metrics::Metrics;
use crate::profile::Profile;
use crate::valve::Valve;

pub const PAGE_LIMIT_MAX: u32 = 100;
const PROBE_CANDIDATES: usize = 3;
/// The shortest gap between two lazy repairs of one namespace, so a failing service cannot be
/// turned into a re-negotiation storm.
const REPAIR_INTERVAL: Duration = Duration::from_secs(60);
/// Ceiling for a total-less walk: a server that never returns a short page must not
/// spin this loop forever.
const MAX_PAGES: u32 = 10_000;
/// The rate-budget key every [`Client::get_path`] shares. Its path is a runtime string, so it
/// has no catalog template to key a bucket by, and one key for the lot is the honest name for
/// what it is: a single caller, the Disaster Recovery sampler, which paces itself.
const GET_PATH_TEMPLATE: &str = "GET (path)";
/// The widest this client ever runs, whatever a Prism Central advertises. The narrower bound is
/// the host tier itself: see [`Client::fan_out`].
const MAX_FAN_OUT: usize = 4;
/// How many times one request may wait for the session it was riding to be renewed before it
/// gives up and hands the 401 to its caller.
///
/// More than one because a renewal is not instantaneous and a session can end again while a
/// request is queued behind the last one - twice over is a busy client against a short-lived
/// session, not a broken credential. Bounded, because an unbounded wait here is a loop.
const RENEWALS: usize = 2;

#[derive(Debug, Clone)]
pub struct ListOptions {
    /// 1..=100; clamped.
    pub limit: u32,
    pub filter: Option<String>,
    pub orderby: Option<String>,
    pub select: Option<String>,
    /// Ids filling the `{...}` placeholders of a nested kind's list path, root first.
    pub parents: Vec<String>,
}

impl Default for ListOptions {
    fn default() -> Self {
        ListOptions {
            limit: PAGE_LIMIT_MAX,
            filter: None,
            orderby: None,
            select: None,
            parents: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct Page {
    pub entities: Vec<Entity>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    pub ext_id: String,
}

/// What an action's 2xx carried. The catalog's `returns` chooses, and a wrong guess degrades
/// rather than erroring: a mutation the server accepted must never read as a failure.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionResult {
    Task(TaskRef),
    Payload(Value),
    Empty,
}

/// Top-level keys a v4 PUT must not carry back. Nested `$objectType` keys stay: they carry the
/// polymorphic discriminator a v4 body needs. `ownerUuid` is absent, because the category PUT
/// requires it.
const READ_ONLY_KEYS: &[&str] = &[
    "$objectType",
    "$reserved",
    "$metadata",
    "extId",
    "links",
    "tenantId",
    "createdBy",
    "creationTime",
    "lastModifiedTime",
];

/// A cluster pinned by a context, resolved from its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterRef {
    pub ext_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceStatus {
    pub name: &'static str,
    /// The catalog's version for this namespace.
    pub version: &'static str,
    /// The version this Prism Central answered at; `None` when no version answered.
    pub pinned: Option<&'static str>,
    pub ok: bool,
    /// What settled it, in one phrase: `probed /vmm/v4.3/ahv/config/vms`, `not served at v4.0`.
    /// The plain fact and nothing about where this row came from - that is `restored`, which is
    /// a flag precisely so that it cannot be prepended to this string twice.
    pub detail: String,
    /// This row is last run's answer, read out of the cache, and nothing this session probed
    /// said it. Always false on anything `negotiate` built.
    pub restored: bool,
}

impl NamespaceStatus {
    /// The namespace answered at `version`: a list, a 403, or any non-404 4xx.
    fn served(ns: &'static Namespace, version: &'static str, detail: String) -> NamespaceStatus {
        NamespaceStatus {
            name: ns.name,
            version: ns.version,
            pinned: Some(version),
            ok: true,
            detail,
            restored: false,
        }
    }

    /// The version is there but the service behind it is not: a 5xx or an undecodable body.
    fn down(ns: &'static Namespace, version: &'static str, detail: String) -> NamespaceStatus {
        NamespaceStatus {
            name: ns.name,
            version: ns.version,
            pinned: Some(version),
            ok: false,
            detail,
            restored: false,
        }
    }

    /// No version answered, so nothing is pinned.
    fn not_served(ns: &'static Namespace, detail: String) -> NamespaceStatus {
        NamespaceStatus {
            name: ns.name,
            version: ns.version,
            pinned: None,
            ok: false,
            detail,
            restored: false,
        }
    }
}

/// What a table or a pane says instead of rows when its list answered 404. Not "the namespace
/// is missing" and not "needs v4.3": the request was made and this is what came back.
///
/// Here rather than in the TUI because [`Availability::ListNotFound`] is the same sentence
/// about the same fact, reached without a request the second time: one 404 is remembered, and
/// every later reader - a greyed menu row, a greyed pane, a refused action - has to read the
/// same words as the table that learned it.
pub const NOT_SERVED: &str = "not served by this Prism Central (HTTP 404)";

/// Whether a kind can be listed on the Prism Central this client negotiated with. Three
/// refusals, in the order they are decided: the namespace, then the kind's version, then what
/// the server has already answered about this very kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Served,
    /// The namespace answered at no version, is down, or was never negotiated; the detail
    /// is the negotiation status text.
    NamespaceUnavailable(String),
    /// The namespace answers, but not at a version that has this kind: the catalog places the
    /// kind's first appearance (`since`) after the version this session pinned. No request is
    /// expected to succeed - `pinned_path_for` leaves such a path at the kind's own version,
    /// which is a version this Prism Central did not answer the probe at - so none is made
    /// unless the user asks for one with [`Client::ask_anyway`].
    NotServedAtPin {
        namespace: &'static str,
        since: &'static str,
        pinned: &'static str,
    },
    /// This kind's top-level list answered 404 earlier in this session. The server's own
    /// verdict, remembered so it is asked for once rather than once a cycle.
    ListNotFound,
}

impl Availability {
    /// Why this Prism Central cannot serve the kind, or `None` when it can. The rule is
    /// spelled once, here, so a greyed menu row, a greyed palette row and a refused action all
    /// read the same.
    pub fn reason(&self) -> Option<String> {
        match self {
            Availability::Served => None,
            Availability::NamespaceUnavailable(_) => Some("namespace not served".to_string()),
            // Both versions, because one of them alone leaves the reader with the wrong
            // question. `not served at multidomain v4.2` said which version this Prism Central
            // settled on and left the reader to guess what would have been needed; naming
            // `v4.3` alone would read as a promise that upgrading delivers the kind, which
            // nothing here knows. Together they state the fact and nothing more: the catalog
            // first saw this path at v4.3, and this session negotiated v4.2. `:try` is how a
            // reader who thinks the prediction is wrong makes the server answer for itself.
            Availability::NotServedAtPin {
                namespace,
                since,
                pinned,
            } => Some(format!(
                "needs {namespace} {since} (this PC pinned {pinned})"
            )),
            Availability::ListNotFound => Some(NOT_SERVED.to_string()),
        }
    }
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    host: String,
    username: String,
    // Never derive Debug on Client: this field must not appear in logs or error text.
    password: String,
    /// Per namespace, the version negotiation settled on, or the pins a restore adopted. Empty
    /// until one of the two happens.
    ///
    /// Behind an `RwLock` because the client is inside an `Arc` from the moment `connect`
    /// returns: `adopt_pins`, the lazy repair of one namespace, and `$select`'s per-kind
    /// give-up all write after that point. **Never held across an await**: every use below
    /// takes the guard, copies what it needs, and drops it.
    pins: std::sync::RwLock<HashMap<&'static str, &'static str>>,
    statuses: std::sync::RwLock<Vec<NamespaceStatus>>,
    /// Namespaces whose pin came from a restore rather than from a probe: the only ones the
    /// lazy repair applies to, and the ones it removes itself from once repaired.
    adopted: std::sync::RwLock<HashSet<&'static str>>,
    /// The last repair attempt per namespace, so a failing service cannot be turned into a
    /// re-negotiation storm.
    repaired: std::sync::Mutex<HashMap<&'static str, std::time::Instant>>,
    /// Kinds whose list answered 400 while carrying `$select`: the narrowing is given up for
    /// them for the rest of the session, and every later list of that kind asks for the whole
    /// document. One 400 per kind, at most.
    no_select: std::sync::RwLock<HashSet<&'static str>>,
    /// Kinds whose **top-level list** answered 404, by kind id: what the server has already
    /// said about an endpoint, kept where everything that asks "can this be listed" can see
    /// it. The scheduler decides which 404 counts (a child list's 404 is a gone parent, not a
    /// missing endpoint) and calls [`Client::mark_missing`]; a cycle that then completes calls
    /// [`Client::forget_missing`], so `^r` over a stopped view is a way back and not a
    /// formality.
    ///
    /// Written here rather than only in the store because the store's flag is per *table*: the
    /// same kind appears as a page's pane, a palette row, a menu item, an action's target and
    /// the stats counters, and every one of them would otherwise re-learn the 404 for itself,
    /// once a cycle.
    missing: std::sync::RwLock<HashSet<&'static str>>,
    /// Kinds the user asked for anyway, by kind id: see [`Client::ask_anyway`]. Only a
    /// person's own request puts a kind here.
    asked: std::sync::RwLock<HashSet<&'static str>>,
    /// The preventive rate budgets. One lock over both halves: `acquire` has to consult the
    /// ceiling and the operation together, and a window must not roll in between.
    buckets: std::sync::Mutex<Buckets>,
    /// The single-flight authentication: see [`Auth`].
    auth: Auth,
    /// The session cookies, shared with `http`'s cookie layer: see [`SessionJar`].
    jar: std::sync::Arc<SessionJar>,
    /// The last resort: see [`Valve`].
    valve: Valve,
    /// What the status line's meter reads. An `Arc` because the TUI holds the client behind
    /// one and reads the meter every frame, and because `send` writes to it from whatever task
    /// the request is on.
    metrics: std::sync::Arc<Metrics>,
}

/// How many sessions this client may fail to get any use out of before it stops trying.
///
/// Two ways to fail to get use out of one, and both count here: a cookie that is refused
/// without ever having authenticated a request, and a presentation that opens no session at
/// all. Either way this Prism Central has nothing to reuse - its cookie does not authenticate
/// this API, or it sets none - and treating that as expiry would send every single request
/// through the single-flight gate, one at a time, for the life of the session. So it is given
/// up, and every request goes back to carrying the credential with no gate at all, which is
/// what this client did before session reuse existed.
///
/// The rule is written on two facts that only ever move one way - "a cookie has been answered
/// at least once" and "how many sessions came to nothing" - and it is evaluated when the jar
/// is *read*, not when a session is refused. That matters under concurrency: a burst can have
/// a refusal come back before the successes it went out with, and deciding at the refusal
/// would read "never answered" for a session that was working perfectly well. Deciding on a
/// monotonic pair, lazily, makes that misreading cost at most one extra presentation instead of
/// giving up session reuse for the rest of the run.
const GIVE_UP_RENEWALS: u32 = 2;

/// The session cookies a Prism Central hands out, replayed instead of the password.
///
/// A pc.7.6 Prism Central answers a plain v4 Basic request with three of them -
/// `NTNX_MERCURY_IAM_SESSION`, `NTNX_MERCURY_IAM_REFRESH_TOKEN`, `NTNX_IAM_SESSION`, all
/// `Secure; HttpOnly; SameSite=Lax` and expiring about fifteen minutes out. None of this is in
/// any spec file, so the whole mechanism is opportunistic: a Prism Central that sets no cookie
/// leaves every request carrying the credential, which is what this client did anyway.
///
/// Written rather than `reqwest`'s own `Jar` for two reasons: the jar has
/// to be **clearable**, so an expired session can be dropped rather than replayed into a second
/// 401, and it has to be **countable**, so `N` requests holding the same stale cookie can be
/// made to produce one re-authentication between them rather than `N`. `generation` is what
/// makes the second exact.
///
/// Attributes are ignored, `Domain`, `Path` and `Secure` included, and that is sound here and
/// nowhere else: this jar belongs to one `Client`, a `Client` talks to exactly one Prism
/// Central, and every request it makes is under `/api`. The far end is the authority on when a
/// session has ended, and it says so with a 401.
#[derive(Debug)]
struct SessionJar {
    /// The one origin this jar will hand a cookie to, or take one from. Defence in depth
    /// behind [`Client::connect`]'s redirect policy: the policy is what actually closes the
    /// leak, and this is what keeps it closed if the policy is ever relaxed.
    ///
    /// It is read in three places, and the third is the one that is easy to miss. `cookies`
    /// and `set_cookies` are reqwest's cookie layer asking. But the session does not travel by
    /// that layer - `attempt` sets the `Cookie` header itself, from [`SessionJar::ride`], which
    /// never sees a URL - so [`may_carry`] asks a third time, against the built request, which
    /// is the only place the address this session is about to ride to is known.
    origin: Origin,
    inner: std::sync::RwLock<Jarred>,
}

/// Whether a request may go out as it stands: a session may ride only to the origin whose jar
/// it came from, and a request carrying the credential has already been decided elsewhere.
///
/// Unreachable today, twice over, and written to stay that way. Every URL this client builds
/// comes from [`Client::raw_url`], which formats the profile's own base, and a cookie can only
/// be in the jar at all if [`SessionJar::set_cookies`] accepted it for that same origin. What
/// it guards against is the future: a `Location` followed by hand, an `_links` href, a `base`
/// made mutable. Any of those would hand `attempt` a URL from the far end, and without this the
/// live session would go out on it with nothing to say otherwise.
fn may_carry(origin: &Origin, url: &Url, presented: bool) -> bool {
    presented || origin.is(url)
}

/// Scheme, host and port: the three things that have to match before a session cookie may
/// cross between a request and this jar.
#[derive(Debug)]
struct Origin {
    scheme: &'static str,
    host: String,
    port: u16,
}

impl Origin {
    fn of(profile: &Profile) -> Origin {
        Origin {
            scheme: if profile.plain_http { "http" } else { "https" },
            host: profile.host.clone(),
            port: profile.port,
        }
    }

    /// Whether `url` is this Prism Central and not some other host. `port_or_known_default`
    /// rather than `port`, because a `Url` drops `:443` from an https address and would
    /// otherwise never match a profile that spells it out.
    fn is(&self, url: &Url) -> bool {
        url.scheme() == self.scheme
            && url.port_or_known_default() == Some(self.port)
            && url
                .host_str()
                .is_some_and(|h| h.eq_ignore_ascii_case(&self.host))
    }
}

#[derive(Debug, Default)]
struct Jarred {
    /// `name` to `value`, in name order so the header is stable.
    cookies: BTreeMap<String, String>,
    /// Bumped by [`SessionJar::expire`] alone. A request captures it before it goes out and
    /// hands it back with its 401, so only the first of `N` requests holding one stale session
    /// is the one that renews it.
    generation: u64,
    /// Whether a request riding a session from this Prism Central has ever been answered. Once
    /// true it stays true: one answer settles that cookies authenticate this API.
    ever_answered: bool,
    /// How many sessions have been refused. Only ever grows.
    renewals: u32,
}

impl SessionJar {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Jarred> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Jarred> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether session reuse is still worth attempting against this Prism Central: see
    /// [`GIVE_UP_RENEWALS`].
    ///
    /// Safe to read outside the auth gate because both facts behind it only move one way. A
    /// `false` here is the degraded mode, and it is the one case in which a request presents
    /// the credential without holding the permit.
    fn usable(&self) -> bool {
        let jar = self.read();
        jar.ever_answered || jar.renewals < GIVE_UP_RENEWALS
    }

    /// The session to ride: the exact `Cookie` header, and the generation it belongs to, read
    /// under one lock. `None` when there is nothing to ride - an empty jar, or a Prism Central
    /// whose cookies this client has given up on.
    ///
    /// Both halves have to come out together, and the header has to travel *with the request*
    /// rather than be looked up again by the cookie layer at dispatch. The jar can be emptied
    /// by any other task at any moment, and a request that went out carrying neither a session
    /// nor a credential would be answered 401, be read as one more session ending, and buy a
    /// second renewal - and a second password - for a single expiry.
    fn ride(&self) -> Option<(u64, header::HeaderValue)> {
        let jar = self.read();
        if jar.cookies.is_empty() || !(jar.ever_answered || jar.renewals < GIVE_UP_RENEWALS) {
            return None;
        }
        let joined = jar
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        header::HeaderValue::from_str(&joined)
            .ok()
            .map(|v| (jar.generation, v))
    }

    /// A request riding a session was answered. What [`SessionJar::usable`] reads to tell a
    /// Prism Central whose sessions end from one whose cookies authenticate nothing.
    fn answered(&self) {
        self.write().ever_answered = true;
    }

    /// A presentation was answered. One that left the jar empty opened no session, and that
    /// counts exactly like a session refused: both are this Prism Central offering nothing to
    /// reuse, and [`GIVE_UP_RENEWALS`] of either settles it.
    ///
    /// Without this the client would take the gate for every single request against a Prism
    /// Central that sets no cookie, because every request would find the jar empty - one
    /// request at a time, for the life of the session.
    fn presented(&self) {
        let mut jar = self.write();
        if jar.cookies.is_empty() {
            jar.renewals = jar.renewals.saturating_add(1);
        }
    }

    /// The session behind `generation` was refused: drop it, so the next request through the
    /// gate finds an empty jar and renews. A caller whose generation has already moved on has
    /// had its renewal made for it by somebody else and drops nothing.
    ///
    /// The refusal is counted whether or not this caller owned it, because [`GIVE_UP_RENEWALS`]
    /// is about the Prism Central rather than about one request.
    fn expire(&self, generation: u64) {
        let mut jar = self.write();
        jar.renewals = jar.renewals.saturating_add(1);
        if jar.generation != generation {
            return;
        }
        jar.cookies.clear();
        jar.generation += 1;
    }
}

impl reqwest::cookie::CookieStore for SessionJar {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &header::HeaderValue>, url: &Url) {
        // A `Set-Cookie` from anywhere but this Prism Central is not banked: banking it would
        // replay another host's cookie to the configured one on the next request.
        if !self.origin.is(url) {
            return;
        }
        let mut jar = self.write();
        if !(jar.ever_answered || jar.renewals < GIVE_UP_RENEWALS) {
            return;
        }
        for value in headers {
            let Ok(text) = value.to_str() else { continue };
            // `name=value` up to the first `;`; the attributes after it are the far end's
            // business, and see the type's note on why they are not this jar's.
            let Some((name, rest)) = text.split_once('=') else {
                continue;
            };
            let name = name.trim().to_string();
            let value = rest.split(';').next().unwrap_or("").trim().to_string();
            let deleted = value.is_empty()
                || text
                    .to_ascii_lowercase()
                    .split(';')
                    .any(|a| a.trim() == "max-age=0");
            if deleted {
                jar.cookies.remove(&name);
            } else {
                jar.cookies.insert(name, value);
            }
        }
    }

    /// Only ever reached by a request that is presenting the credential: one that is riding a
    /// session carries the cookie [`SessionJar::ride`] handed it, and `reqwest` leaves a
    /// `Cookie` header that is already set alone.
    fn cookies(&self, url: &Url) -> Option<header::HeaderValue> {
        if !self.origin.is(url) {
            return None;
        }
        self.ride().map(|(_, header)| header)
    }
}

/// What this Prism Central has said about the credential this client holds.
///
/// Three states, and every transition is one way: unproven until an answer proves it, and the
/// answer that proves it wrong is final. It says nothing about the *session* - whether there is
/// one to ride is the jar's business, and [`Client::attempt`] asks the jar - so nothing here has
/// to be unwound when a session ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthState {
    /// No answer has settled the credential either way. The gate is shut behind the first
    /// request, so whatever concurrency the caller asked for, one password goes out.
    Unauthenticated,
    /// A Prism Central answered something other than 401, so the credential is good. A request
    /// with a session to ride skips the gate from here on.
    Established,
    /// A request that carried the credential was answered 401. Nothing goes out after this:
    /// [`Client::send`] returns [`PrismError::Auth`] without touching the network.
    Rejected,
}

/// The single-flight authentication.
///
/// Concurrent probes must not each carry the password. Against a Prism Central with a lockout
/// policy that is one authentication event per request in flight, and a **wrong** password is
/// one failure per request. The semaphore has one permit, and the request holding it is the
/// only one that may present a credential: see [`Client::carry`].
struct Auth {
    /// A `Mutex` held for nanoseconds and never across an await, on the precedent of
    /// `Client::buckets`.
    state: std::sync::Mutex<AuthState>,
    /// One permit, taken by every request that has no session to ride.
    gate: tokio::sync::Semaphore,
}

impl Auth {
    fn new() -> Auth {
        Auth {
            state: std::sync::Mutex::new(AuthState::Unauthenticated),
            gate: tokio::sync::Semaphore::new(1),
        }
    }

    /// A poisoned state is taken as it stands, for the reason `Client::buckets` gives. The
    /// conservative reading is the one already there: a panic cannot invent an `Established`.
    fn state(&self) -> AuthState {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What one answer proved about the credential. `presented` says whether this request
    /// actually carried one: a 401 on a request that did is the credential being refused, and
    /// there is no state in which that becomes untrue again.
    fn settle(&self, presented: bool, status: StatusCode) {
        let next = match (presented, status == StatusCode::UNAUTHORIZED) {
            (true, true) => AuthState::Rejected,
            // Anything that is not a 401 - a 200, a 404, a 403, a 500 - is the far end having
            // authenticated the request before it routed it. A 403 in particular: the account
            // is real and this is what it may not read.
            (_, false) => AuthState::Established,
            // A 401 on a request that carried no credential settles nothing here; the caller
            // decides what it means.
            (false, true) => return,
        };
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *guard == AuthState::Rejected {
            return;
        }
        *guard = next;
    }
}

/// How one trip through [`Client::attempt`] is to treat the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// The request as its caller made it: ride the session if there is one, and go through the
    /// gate only when there is not.
    First,
    /// Coming back round after the session it rode had ended. It goes through the gate even
    /// when the credential is established, so that it waits for whoever is renewing instead of
    /// riding a second session that has already gone.
    Renewing,
    /// The last go: authenticate rather than ride, whatever the jar holds. The answer is then
    /// definitive - a 200, or a 401 that means the credential really is refused - so a request
    /// whose session kept ending can never come back to its caller as a refused credential.
    Authenticate,
}

/// What one request carries to prove who it is. Exactly one of the two, never neither: a request
/// with neither would be answered 401, and that 401 reads as a session ending.
enum Carry {
    /// The credential. Only ever built while this request holds the auth permit, or once
    /// session reuse has been given up for good - see [`Client::carry`].
    Credential,
    /// This exact session cookie, and the generation it belongs to.
    Session(u64, header::HeaderValue),
}

/// What one trip through [`Client::attempt`] settled.
enum Attempt {
    Answered((StatusCode, HeaderMap, Vec<u8>)),
    /// A 401 on a request that carried no credential: the session it was riding has ended, and
    /// this request has not been answered yet.
    SessionEnded,
}

/// The host ceiling and one bucket per operation.
#[derive(Debug)]
struct Buckets {
    /// Every request passes here first, whatever its own tier says.
    host: Bucket,
    /// Keyed by method *and* path template. By the template because that is what the server
    /// meters: every VM shares the power-off budget. By the method too because the specs
    /// declare a tier per *operation*, and one template carries several - `/vms/{extId}` is the
    /// read at two a second, the PUT and the DELETE at five - so a single key per template
    /// would let whichever call arrived first fix the tier for all of them, over the declared
    /// budget in one order and needlessly under it in the other.
    per_op: HashMap<(HttpMethod, &'static str), Bucket>,
}

impl Client {
    pub fn connect(profile: &Profile, password: &str) -> Result<Client, PrismError> {
        let mut builder = reqwest::Client::builder()
            .gzip(true)
            // Prism's `/api/*` has no legitimate redirect, and following one is how the session
            // cookie leaves this Prism Central: `reqwest` builds
            // `FollowRedirect::with_policy(CookieService::new(..))`, so `tower-http` strips the
            // `Cookie` header on the hop and the cookie layer immediately puts it back for the
            // host the `Location` named. A 3xx therefore comes back as the answer it is.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .tls_danger_accept_invalid_certs(!profile.verify_tls);
        if let Some(path) = &profile.ca_bundle {
            let pem = std::fs::read(path)
                .map_err(|e| PrismError::Tls(format!("reading {}: {e}", path.display())))?;
            let cert =
                reqwest::Certificate::from_pem(&pem).map_err(|e| PrismError::Tls(describe(&e)))?;
            builder = builder.tls_certs_merge([cert]);
        }
        let jar = std::sync::Arc::new(SessionJar {
            origin: Origin::of(profile),
            inner: std::sync::RwLock::new(Jarred::default()),
        });
        let http = builder
            .cookie_provider(std::sync::Arc::clone(&jar))
            .build()
            .map_err(|e| PrismError::Tls(describe(&e)))?;
        Ok(Client {
            http,
            base: profile.base_url(),
            host: profile.host.clone(),
            username: profile.username.clone(),
            password: password.to_string(),
            pins: std::sync::RwLock::new(HashMap::new()),
            statuses: std::sync::RwLock::new(Vec::new()),
            adopted: std::sync::RwLock::new(HashSet::new()),
            repaired: std::sync::Mutex::new(HashMap::new()),
            no_select: std::sync::RwLock::new(HashSet::new()),
            missing: std::sync::RwLock::new(HashSet::new()),
            asked: std::sync::RwLock::new(HashSet::new()),
            buckets: std::sync::Mutex::new(Buckets {
                host: Bucket::new(bucket::HOST_START),
                per_op: HashMap::new(),
            }),
            auth: Auth::new(),
            jar,
            valve: Valve::default(),
            metrics: std::sync::Arc::new(Metrics::default()),
        })
    }

    /// `/api` plus `path` exactly as given; a probe names its own version, so it must not be
    /// redirected by an earlier pin. Every other caller passes a path through `pinned_path`
    /// first, and hands that same path to `ok`, so an error names the URL that was requested.
    fn raw_url(&self, path: &str) -> String {
        format!("{}/api{}", self.base, path)
    }

    /// `path` with the namespace's version segment replaced by the pinned one, or unchanged
    /// when there is no pin or the pin equals the version already in the path.
    fn pinned_path(&self, path: &str) -> String {
        let namespace = path.trim_start_matches('/').split('/').next().unwrap_or("");
        match (self.pins_guard().get(namespace), version_in(path)) {
            (Some(pinned), Some(version)) if *pinned != version => with_version(path, pinned),
            _ => path.to_string(),
        }
    }

    /// `pinned_path` for a request about `kind`: a kind newer than the namespace's pin has no
    /// path at the pinned version, so it is asked for where the catalog says it exists, and
    /// the answer (a 404 on an old PC, a 200 on a PC that serves newer kinds under an older
    /// pin, as some do) is the server's to give.
    fn pinned_path_for(&self, kind: &Kind, path: &str) -> String {
        match self.pins_guard().get(kind.namespace) {
            Some(pinned) if !kind.served_at(pinned) => path.to_string(),
            _ => self.pinned_path(path),
        }
    }

    pub async fn list_page(
        &self,
        kind: &'static Kind,
        page: u32,
        opts: &ListOptions,
    ) -> Result<Page, PrismError> {
        self.list_page_at(kind, page, opts, None).await
    }

    /// `list_page` at an explicit API version, bypassing the pins: how negotiation probes a
    /// version. `None` means the catalog path, rewritten through the pins as usual.
    async fn list_page_at(
        &self,
        kind: &'static Kind,
        page: u32,
        opts: &ListOptions,
        version: Option<&str>,
    ) -> Result<Page, PrismError> {
        let limit = opts.limit.clamp(1, PAGE_LIMIT_MAX);
        let mut query: Vec<(&str, String)> = Vec::new();
        if kind.list_params.page {
            query.push(("$page", page.to_string()));
        }
        if kind.list_params.limit {
            query.push(("$limit", limit.to_string()));
        }
        if let (true, Some(f)) = (kind.list_params.filter, &opts.filter) {
            query.push(("$filter", f.clone()));
        }
        if let (true, Some(o)) = (kind.list_params.orderby, &opts.orderby) {
            query.push(("$orderby", o.clone()));
        }
        let parents: Vec<&str> = opts.parents.iter().map(String::as_str).collect();
        let scoped = scoped_path(kind, kind.list_path, &parents, None)?;
        let path = match version {
            Some(v) => with_version(&scoped, v),
            None => self.pinned_path_for(kind, &scoped),
        };
        // The narrowing the caller asked for, if this endpoint declares one and this Prism
        // Central has not already refused it. Never `kind.select`: what is narrowed is the
        // scheduler's decision, and a `$select` applied to every list from down here would
        // reach `can_i` and the version probes, which come through with default options.
        let mut select = match (kind.list_params.select, &opts.select) {
            (true, Some(s)) if !self.select_refused(kind) => Some(s.clone()),
            _ => None,
        };
        loop {
            let mut query = query.clone();
            if let Some(s) = &select {
                query.push(("$select", s.clone()));
            }
            let req = self.http.get(self.raw_url(&path)).query(&query);
            // After `scoped_path`: a caller error must not cost a token, still less a whole
            // window asleep before it is reported. `send` is what spends it.
            let (status, headers, body) = self
                .send(&HttpMethod::GET, kind.list_path, kind.rate, req)
                .await?;
            // The extraction is inside the same arm: a body that parses as JSON and carries no
            // usable `data` is a `Decode`, and it is raised here rather than by `ok` so that
            // the caller sees one error for the request rather than a success it cannot read.
            let e = match Self::ok(status, &headers, &body, &path).and_then(envelope::list_items) {
                Ok((items, total)) => {
                    return Ok(Page {
                        entities: items
                            .into_iter()
                            .map(|v| Entity::new(kind, v, None))
                            .collect(),
                        total,
                    });
                }
                Err(e) => e,
            };
            // A 400 on a list carrying `$select` is this Prism Central saying the narrowing is
            // wrong for this endpoint, whatever the spec declared. The table is worth more than
            // the bytes: ask for the whole document instead, once, and remember for the rest of
            // the session so the next cycle does not spend a request rediscovering it.
            if select.is_some() && matches!(e, PrismError::Api { status: 400, .. }) {
                self.refuse_select(kind);
                select = None;
                continue;
            }
            // `version.is_none()` excludes the probes themselves: a 404 during negotiation is
            // the answer negotiation is asking for, not a pin to repair.
            if version.is_none() {
                self.repair_after(kind.namespace, &e).await;
            }
            return Err(e);
        }
    }

    /// Page 0 first, then the remaining pages with bounded concurrency, in order.
    pub async fn list_all(
        &self,
        kind: &'static Kind,
        opts: &ListOptions,
    ) -> Result<Vec<Entity>, PrismError> {
        let first = self.list_page(kind, 0, opts).await?;
        let limit = u64::from(opts.limit.clamp(1, PAGE_LIMIT_MAX));
        let mut all = first.entities;
        if !kind.list_params.page {
            return Ok(all);
        }
        let limit_usize = limit as usize;
        match first.total {
            // A total we can trust: fan the remaining pages out, then keep them in page order.
            Some(total) if total > limit => {
                let pages = page_count(total, limit);
                // The same ceiling the total-less walk below has, on the branch that lacked
                // it. A Prism Central that miscounts its own list is the one input this arm
                // cannot survive: it fanned out over every page the total named, which for a
                // `totalAvailableResults` of 2^63-1 is an endless crawl at the pacing's three
                // requests a second, with nothing on the screen to say so.
                if pages > MAX_PAGES {
                    return Err(PrismError::Decode(format!(
                        "{} reported {total} rows, which is more than {MAX_PAGES} pages",
                        kind.id
                    )));
                }
                let rest: Vec<Page> = stream::iter(1..pages)
                    .map(|p| self.list_page(kind, p, opts))
                    .buffered(self.fan_out())
                    .try_collect()
                    .await?;
                for page in rest {
                    all.extend(page.entities);
                }
            }
            Some(_) => {}
            // No total: walk one page at a time until a short or empty page ends the list.
            None => {
                let mut page = 1;
                while all.len() % limit_usize == 0 && !all.is_empty() {
                    if page >= MAX_PAGES {
                        return Err(PrismError::Decode(format!(
                            "{} returned more than {MAX_PAGES} pages without a total",
                            kind.id
                        )));
                    }
                    let next = self.list_page(kind, page, opts).await?;
                    if next.entities.is_empty() {
                        break;
                    }
                    all.extend(next.entities);
                    page += 1;
                }
            }
        }
        Ok(all)
    }

    /// Match a cluster by name (exact, then case-insensitive). Zero or several matches are errors
    /// that list the candidates.
    pub async fn resolve_cluster(&self, name: &str) -> Result<ClusterRef, PrismError> {
        let kind = nutsh_catalog::kind("clustermgmt.config.Cluster")
            .ok_or_else(|| PrismError::Catalog("catalog has no cluster kind".into()))?;
        let clusters = self.list_all(kind, &ListOptions::default()).await?;
        let exact: Vec<&Entity> = clusters.iter().filter(|c| c.name == name).collect();
        let matches = if exact.is_empty() {
            clusters
                .iter()
                .filter(|c| c.name.eq_ignore_ascii_case(name))
                .collect::<Vec<_>>()
        } else {
            exact
        };
        match matches.as_slice() {
            [one] => Ok(ClusterRef {
                ext_id: one.ext_id.clone(),
                name: one.name.clone(),
            }),
            // Not `NotFound`: nothing 404'd, the list came back fine and held no such name.
            [] => Err(PrismError::Unresolved(format!(
                "cluster {name:?} not found; known: {}",
                name_and_id(&clusters)
            ))),
            many => Err(PrismError::Ambiguous(format!(
                "cluster {name:?} is ambiguous: {}",
                name_and_id(many.iter().copied())
            ))),
        }
    }

    pub async fn get(&self, kind: &'static Kind, ext_id: &str) -> Result<Entity, PrismError> {
        self.get_in(kind, &[], ext_id).await
    }

    /// `get` for a nested kind: `parents` fill the path's leading placeholders, `ext_id` the last.
    pub async fn get_in(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
    ) -> Result<Entity, PrismError> {
        let template = kind
            .get_path
            .ok_or_else(|| PrismError::Catalog(format!("{} has no get endpoint", kind.id)))?;
        let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
        let path = scoped_path(kind, template, &parents, Some(ext_id))?;
        let path = self.pinned_path_for(kind, &path);
        // The kind's tier is the list GET's; the catalog declares no separate one for the
        // read, and it is the closest budget there is.
        let (status, headers, body) = self
            .send(
                &HttpMethod::GET,
                template,
                kind.rate,
                self.http.get(self.raw_url(&path)),
            )
            .await?;
        let env = Self::ok(status, &headers, &body, &path)?;
        // `If-Match` uses strong comparison, so a weak validator is stored without its `W/`
        // prefix: sending `W/"x"` back would never match.
        let etag = headers
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("W/").unwrap_or(v).to_string());
        Ok(Entity::new(kind, envelope::one(env)?, etag))
    }

    /// A GET at an API path the catalog does not name, returning the envelope's `data`.
    ///
    /// It exists for one caller: the Disaster Recovery page's sampler.
    /// `GET /dataprotection/v4.4/config/protected-resources/{extId}` is get-by-id only - there
    /// is no list endpoint, which is why the kind is absent from the catalog - and
    /// `replicationStates[].replicationStatus` is reachable one entity at a time.
    ///
    /// The path is version-pinned like any other, so a Prism Central pinned at an older
    /// `dataprotection` is asked at the version it serves.
    pub async fn get_path(&self, path: &str) -> Result<Value, PrismError> {
        let path = self.pinned_path(path);
        // Every request passes the host ceiling first, whatever its own tier says, and the
        // operation's own budget has to exist for a 429 to drain it. The tier is the catalog's
        // default because there is no kind here to read one from - the sampler's one request
        // every 200 ms never reaches it either way.
        let (status, headers, body) = self
            .send(
                &HttpMethod::GET,
                GET_PATH_TEMPLATE,
                RateLimit::DEFAULT,
                self.http.get(self.raw_url(&path)),
            )
            .await?;
        let env = Self::ok(status, &headers, &body, &path)?;
        envelope::one(env)
    }

    /// Run a catalog action. `parents` fill the path's leading placeholders and `ext_id` the
    /// entity's; a count mismatch is a programming error, reported as `Catalog`. Fetches the
    /// entity first when the action needs an ETag.
    pub async fn act(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
        action: &str,
        body: Option<Value>,
    ) -> Result<ActionResult, PrismError> {
        self.act_with_etag(kind, parents, ext_id, action, body, None)
            .await
    }

    /// `act` with the validator already in hand. A caller that read the entity itself - `update`
    /// builds its body from one - passes the ETag of *that* read: letting this method fetch a
    /// second, newer one would send the older body under a fresh `If-Match`, so a write that
    /// landed in between would be overwritten rather than refused with a 412. `None` means
    /// "fetch it here", which is what `act` asks for.
    async fn act_with_etag(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
        action: &str,
        body: Option<Value>,
        etag: Option<String>,
    ) -> Result<ActionResult, PrismError> {
        let a = kind
            .action(action)
            .ok_or_else(|| PrismError::Catalog(format!("{} has no action {action}", kind.id)))?;
        if a.needs_body && body.is_none() {
            return Err(PrismError::Catalog(format!(
                "{action} on {} requires a request body",
                kind.id
            )));
        }
        if !a.takes_body && body.is_some() {
            return Err(PrismError::Catalog(format!(
                "{action} on {} takes no request body",
                kind.id
            )));
        }
        // Not `scope`: `Action::scope` is the list of placeholder *names*, this is the ids.
        let parent_ids: Vec<&str> = parents.iter().map(String::as_str).collect();
        // `scoped_path` already names the kind, the template and both counts; only the action
        // is missing, and a caller that passed the wrong number of ids needs to know which.
        let filled = scoped_path(
            kind,
            a.path,
            &parent_ids,
            a.names_entity().then_some(ext_id),
        )
        .map_err(|e| match e {
            PrismError::Catalog(m) => PrismError::Catalog(format!("{action} on {m}")),
            other => other,
        })?;
        let path = self.pinned_path_for(kind, &filled);
        let method = match a.method {
            Method::Get => HttpMethod::GET,
            Method::Post => HttpMethod::POST,
            Method::Put => HttpMethod::PUT,
            Method::Patch => HttpMethod::PATCH,
            Method::Delete => HttpMethod::DELETE,
        };
        let mut req = self
            .http
            .request(method.clone(), self.raw_url(&path))
            .header("NTNX-Request-Id", uuid::Uuid::new_v4().to_string());
        if a.needs_etag {
            let etag = match etag {
                Some(tag) => tag,
                None => {
                    // Say which fetch failed: a 404 here is the entity, not the action endpoint.
                    let current =
                        self.get_in(kind, parents, ext_id)
                            .await
                            .map_err(|e| match e {
                                PrismError::NotFound(m) => PrismError::NotFound(format!(
                                    "{} {ext_id} (before {action}): {m}",
                                    kind.id
                                )),
                                other => other,
                            })?;
                    current.etag.ok_or_else(|| missing_etag(kind, ext_id))?
                }
            };
            req = req.header(header::IF_MATCH, etag);
        }
        if let Some(b) = &body {
            req = req.json(b);
        }
        // After the refusals and after the ETag read, so a token is spent only once this
        // request is certain to go out: a 404 on that read must not cost the action one.
        let (status, headers, resp) = self.send(&method, a.path, a.rate, req).await?;
        // Only `act` knows what was being changed, so it builds the 412 error itself.
        if status == StatusCode::PRECONDITION_FAILED {
            return Err(PrismError::Conflict {
                kind: kind.id,
                ext_id: ext_id.to_string(),
                action: action.to_string(),
            });
        }
        let env = Self::ok(status, &headers, &resp, &path)?;
        Ok(result_of(a.returns, env.data, kind.id, action))
    }

    /// Fetch, merge `changes` over the top-level keys (a nested object is replaced, not merged),
    /// strip the read-only keys, and PUT the whole object: v4 PUT is full-replace.
    pub async fn update(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
        changes: Value,
    ) -> Result<ActionResult, PrismError> {
        // Refused before the GET: anything but an object would merge nothing and PUT the entity
        // back unchanged, which burns a task and reads as success.
        let Value::Object(changes) = changes else {
            return Err(PrismError::Catalog(format!(
                "{} {ext_id}: update changes must be a JSON object",
                kind.id
            )));
        };
        let current = self.get_in(kind, parents, ext_id).await?;
        // One read serves both halves, the body and the `If-Match`; every `update` action in the
        // catalog is a PUT that needs one, so a missing ETag is an error here, not a second GET.
        let etag = Some(current.etag.ok_or_else(|| missing_etag(kind, ext_id))?);
        let mut body = current.raw;
        let Some(object) = body.as_object_mut() else {
            return Err(PrismError::Decode(format!(
                "{} {ext_id} is not an object",
                kind.id
            )));
        };
        object.extend(changes);
        // After the merge, so `changes` cannot smuggle `extId` or `$objectType` back into the body.
        for key in READ_ONLY_KEYS {
            object.remove(*key);
        }
        self.act_with_etag(kind, parents, ext_id, "update", Some(body), etag)
            .await
    }

    /// `delete` is modelled as an action, so callers need not know that DELETE is one.
    pub async fn delete(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
    ) -> Result<ActionResult, PrismError> {
        self.act(kind, parents, ext_id, "delete", None).await
    }

    /// Route by what the last run negotiated, with **zero** requests.
    ///
    /// `statuses` are the restored **positives only**, and the asymmetry is deliberate: a
    /// false "served" costs one 404, which the lazy repair below then corrects; a false "not
    /// served" suppresses a whole pane or sidebar group with no request ever made, so nothing
    /// can correct it.
    ///
    /// So a namespace with no restored status is *unknown*, not unavailable: it is adopted
    /// with no pin, [`Client::is_served`] and [`Client::availability`] answer for it as though
    /// it were served, and its first list either answers at the catalog's version or fails
    /// into the repair below. That is what keeps a service that was down when the cache was
    /// written - or a whole restore whose statuses have aged past their day - from being
    /// invisible for the rest of that cache's life, with no request left that could correct it.
    ///
    /// A pin naming a namespace the catalog no longer declares, or a version that namespace no
    /// longer offers, is dropped on its own; the rest stand, and that namespace is left
    /// unknown, which is what has it negotiated lazily on its first failure.
    pub fn adopt_pins(&self, pins: &BTreeMap<String, String>, statuses: Vec<NamespaceStatus>) {
        let mut resolved: HashMap<&'static str, &'static str> = HashMap::new();
        for (name, version) in pins {
            let Some(ns) = NAMESPACES.iter().find(|n| n.name == *name) else {
                continue;
            };
            let Some(v) = ns.versions.iter().find(|v| **v == version.as_str()) else {
                continue;
            };
            resolved.insert(ns.name, v);
        }
        let statuses: Vec<NamespaceStatus> = statuses.into_iter().filter(|s| s.ok).collect();
        // Everything this restore did not *probe*: the namespaces it pinned, and the ones it
        // says nothing about. Both are the lazy repair's to correct, and nothing outside this
        // set may be re-probed by it.
        let adopted: HashSet<&'static str> = NAMESPACES
            .iter()
            .map(|ns| ns.name)
            .filter(|name| resolved.contains_key(name) || !statuses.iter().any(|s| s.name == *name))
            .collect();
        *self
            .pins
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = resolved;
        *self
            .statuses
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = statuses;
        *self
            .adopted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = adopted;
    }

    /// A list failed under an **adopted** pin in a way that means "not there" or "down at this
    /// version": re-negotiate that namespace alone, at most once per 60 s.
    async fn repair_after(&self, namespace: &'static str, e: &PrismError) {
        if !worth_repairing(e) {
            return;
        }
        if !self.is_adopted(namespace) {
            return;
        }
        {
            let mut repaired = self
                .repaired
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = std::time::Instant::now();
            if let Some(last) = repaired.get(namespace)
                && now.duration_since(*last) < REPAIR_INTERVAL
            {
                return;
            }
            repaired.insert(namespace, now);
        }
        tracing::debug!(
            namespace,
            "restored pin did not answer; re-negotiating it alone"
        );
        // Boxed because this closes a cycle - a list drives a probe, which is a list - and an
        // async future may not contain itself.
        let _ = Box::pin(self.negotiate_namespace(namespace)).await;
    }

    /// Settles, per namespace, which API version this Prism Central serves: the catalog's
    /// versions newest first, one `$limit=1` list per try, the first non-404 answer pins the
    /// version for every later request through `url`. Runs once per session.
    pub async fn negotiate(&self) -> Result<Vec<NamespaceStatus>, PrismError> {
        // Bad credentials and an unreachable host are not per-namespace verdicts: the first
        // fatal error ends negotiation and the probes still in flight are dropped, so a dead
        // host fails once and at once rather than twenty times behind twenty timeouts.
        // The boxing is load-bearing, not style: with the opaque probe futures inside a
        // stream combinator, any caller that needs this future to be `Send` - the TUI's
        // `BoxFuture<'static, _>` connect - is rejected with "implementation of `FnOnce` is
        // not general enough" (rust-lang/rust#102211). A `BoxFuture` is a concrete type, so
        // the auto-trait check sees through it.
        let probes: Vec<BoxFuture<'_, Result<NamespaceStatus, PrismError>>> = NAMESPACES
            .iter()
            .map(|ns| Box::pin(self.probe_namespace(ns)) as BoxFuture<'_, _>)
            .collect();
        let statuses: Vec<NamespaceStatus> = stream::iter(probes)
            .buffered(self.fan_out())
            .try_collect::<Vec<NamespaceStatus>>()
            .await?;
        {
            let mut pins = self
                .pins
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *pins = statuses
                .iter()
                .filter_map(|s| Some((s.name, s.pinned?)))
                .collect();
        }
        self.statuses
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone_from(&statuses);
        // Nothing here was adopted: every pin above came from a probe.
        self.adopted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        Ok(statuses)
    }

    /// Negotiation for one namespace, which is all `ctx login` needs to verify a password.
    pub async fn negotiate_namespace(&self, name: &str) -> Result<NamespaceStatus, PrismError> {
        let ns = NAMESPACES
            .iter()
            .find(|n| n.name == name)
            .ok_or_else(|| PrismError::Catalog(format!("no namespace {name}")))?;
        let status = self.probe_namespace(ns).await?;
        {
            let mut pins = self
                .pins
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A namespace that stopped answering must lose its old pin, not keep routing to a
            // version this negotiation could not reach.
            match status.pinned {
                Some(p) => pins.insert(ns.name, p),
                None => pins.remove(ns.name),
            };
        }
        {
            let mut statuses = self
                .statuses
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            statuses.retain(|s| s.name != ns.name);
            statuses.push(status.clone());
        }
        // This pin was probed, so it is no longer one the lazy repair may re-probe.
        self.adopted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(ns.name);
        Ok(status)
    }

    /// The negotiation result, one row per namespace, empty before `negotiate` runs or a
    /// restore adopts one. An owned `Vec` rather than a slice: the rows live behind an
    /// `RwLock` now, and a borrow cannot outlive the guard. One caller, `src/check.rs`, and
    /// one clone per `--check` run.
    pub fn statuses(&self) -> Vec<NamespaceStatus> {
        self.statuses_guard().clone()
    }

    /// The request meter. Cheap to call every frame: it clones nothing and computing a
    /// [`crate::Meter`] out of it is `Metrics::snapshot`.
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// The log line and the ring entry for one request, from the same facts.
    fn trace(
        &self,
        method: &reqwest::Method,
        url: &str,
        carried: Carried,
        began: std::time::Instant,
        paced: bool,
        outcome: Result<StatusCode, &PrismError>,
    ) {
        trace_request(method, url, carried, began, outcome);
        self.metrics.record_call(crate::metrics::Call {
            at: began,
            method: method.to_string(),
            path: path_of(url),
            status: outcome.ok().map(|s| s.as_u16()),
            ms: u32::try_from(began.elapsed().as_millis()).unwrap_or(u32::MAX),
            credential: carried.word(),
            paced,
        });
    }

    /// Whether this Prism Central is worth a list request for `kind`: the standing verdict,
    /// and then what the server has already answered about this very endpoint. The predicate
    /// every *volunteered* request passes through - the stats counters, the DR sampler, the
    /// name warm-up, a page's panes - so none of them re-learns a 404 once a cycle.
    pub fn is_served(&self, kind: &Kind) -> bool {
        matches!(self.list_availability(kind), Availability::Served)
    }

    /// The standing verdict on `kind`, with the reason attached: **no request could succeed**,
    /// because the namespace answers at no version or because this kind is not in the version
    /// it pins. Both are derived from the pins as they stand, so a re-negotiation re-decides
    /// them; neither is remembered, and neither can go stale.
    ///
    /// This is what greys a menu row, a palette row and a refused action - everything a person
    /// asks for by name. A list that answered 404 is deliberately **not** here: see
    /// [`Client::list_availability`].
    pub fn availability(&self, kind: &Kind) -> Availability {
        match self.namespace_availability(kind.namespace) {
            Availability::Served => self.version_availability(kind),
            down => down,
        }
    }

    /// The standing verdict, and then the 404 this session already paid for: what to say
    /// instead of subscribing to this kind's list.
    ///
    /// Apart from [`Client::availability`] because the two are asked by different callers for
    /// different reasons. A 404 is the server's answer about one path at one moment; a person
    /// who opens that table anyway gets the sentence where the rows would be and `^r` to ask
    /// again, which is the way back the scheduler's stop was justified by. Refusing to open it
    /// at all would take that away. What this does refuse is a request *volunteered* on the
    /// user's behalf - a pane that would poll it, a counter that would count it.
    pub fn list_availability(&self, kind: &Kind) -> Availability {
        match self.availability(kind) {
            Availability::Served if self.is_missing(kind) => Availability::ListNotFound,
            verdict => verdict,
        }
    }

    /// Is this namespace reachable: it answered at some version, and that version is pinned.
    ///
    /// A namespace with no status row at all is *unknown* rather than unavailable under a
    /// restore, which routes at the catalog's version and is corrected by the first answer
    /// (see `adopt_pins`); with no restore behind it, nothing has been negotiated and nothing
    /// can be asked.
    pub fn namespace_availability(&self, namespace: &str) -> Availability {
        let known = {
            let guard = self.statuses_guard();
            guard
                .iter()
                .find(|s| s.name == namespace)
                .map(|status| match status.pinned {
                    Some(_) if status.ok => Availability::Served,
                    _ => Availability::NamespaceUnavailable(status.detail.clone()),
                })
        };
        known.unwrap_or_else(|| {
            if self.is_adopted(namespace) {
                // Unknown, not unavailable: see `adopt_pins` for why the two differ.
                Availability::Served
            } else {
                Availability::NamespaceUnavailable("not negotiated".into())
            }
        })
    }

    /// Is this *kind* in the API this Prism Central serves: the catalog places its first
    /// appearance at or before the version its namespace pins.
    ///
    /// Read from the pin as it stands, so it is re-decided the moment a re-negotiation moves
    /// that pin rather than being remembered - the one refusal here that costs nothing to be
    /// wrong about. `pinned_path_for` leaves a kind newer than the pin at its own version, so
    /// the only request this could make is one at a version the namespace has already been
    /// found not to serve: the 404 on the Disaster Recovery page's first pane, once a cycle,
    /// for the whole session.
    ///
    /// The trade it accepts, stated where it is made: a Prism Central that routes one kind's
    /// newer path while answering the probe candidates only at an older one is greyed rather
    /// than asked. That is the catalog's knowledge and not a guess - the path
    /// is absent from the pinned version's own spec - and a stale restored pin is corrected by
    /// any re-negotiation, which re-decides this with it.
    pub fn version_availability(&self, kind: &Kind) -> Availability {
        match self.pins_guard().get(kind.namespace).copied() {
            Some(pinned) if !kind.served_at(pinned) && !self.asked_anyway(kind) => {
                Availability::NotServedAtPin {
                    namespace: kind.namespace,
                    since: kind.since,
                    pinned,
                }
            }
            _ => Availability::Served,
        }
    }

    /// Set the version prediction aside for `kind`: ask this Prism Central for it, once, and
    /// let the answer stand.
    ///
    /// The prediction above is the catalog's, and a good one - it saves a 404 a cycle on the
    /// Disaster Recovery page. But it is still a prediction made before the server has had a
    /// vote, and `pinned_path_for` argues the other side of the same case: a Prism Central may
    /// route a kind's newer path while answering the probes only at an older version. This is
    /// where a person who thinks the prediction is wrong about their Prism Central finds out.
    ///
    /// One attempt is all it buys. A 404 is remembered by [`Client::mark_missing`] and greys
    /// the kind again on the server's own verdict, which is a better reason than the one it
    /// replaced; a 200 is the kind working for the rest of the session. Nothing here retries,
    /// and nothing calls it on the user's behalf.
    pub fn ask_anyway(&self, kind: &Kind) {
        self.asked
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind.id);
    }

    /// Whether [`Client::ask_anyway`] was called for this kind.
    pub fn asked_anyway(&self, kind: &Kind) -> bool {
        self.asked
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(kind.id)
    }

    /// Whether this Prism Central has already refused a `$select` on `kind`.
    pub fn select_refused(&self, kind: &Kind) -> bool {
        self.no_select
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(kind.id)
    }

    /// Give up narrowing `kind` for the rest of the session. One way: a 400 is the endpoint
    /// saying the narrowing is wrong, and nothing about it changes while a session is open.
    fn refuse_select(&self, kind: &Kind) {
        self.no_select
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind.id);
    }

    /// Remember that `kind`'s top-level list answered 404. Called by the scheduler, which is
    /// the only caller that knows which 404 is the endpoint's: see `is_missing_list` there.
    pub fn mark_missing(&self, kind: &Kind) {
        self.missing
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind.id);
    }

    /// Forget it: a cycle over `kind` completed, so whatever the 404 was about is over. The
    /// way back for `^r`, and for a service that was still starting when the session opened.
    pub fn forget_missing(&self, kind: &Kind) {
        self.missing
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(kind.id);
    }

    fn is_missing(&self, kind: &Kind) -> bool {
        self.missing
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(kind.id)
    }

    /// The domain manager's `extId` and `config.buildInfo.version`, in one read, with the
    /// request's error kept. The identity check therefore costs nothing extra: `connect`
    /// already made this request for the header.
    ///
    /// `Ok(None)` is "the endpoint is there and shaped differently": the catalog has no such
    /// kind, the list was empty, or the row carries no `extId`. `Err` is the request itself
    /// refusing, and a caller that made no other request - the restore path of
    /// `session::connect` - is the reason it is not swallowed here: a rejected password or a
    /// refused certificate has to be reportable as itself, not as a session with an empty
    /// header.
    pub async fn pc_identity_result(&self) -> Result<Option<(String, Option<String>)>, PrismError> {
        let Some(kind) = nutsh_catalog::kind("prism.config.DomainManager") else {
            return Ok(None);
        };
        let opts = ListOptions {
            limit: 1,
            ..Default::default()
        };
        let page = self.list_page(kind, 0, &opts).await?;
        let Some(first) = page.entities.first() else {
            return Ok(None);
        };
        let ext_id = first.ext_id.clone();
        if ext_id.is_empty() {
            return Ok(None);
        }
        let version = first
            .raw
            .pointer("/config/buildInfo/version")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Some((ext_id, version)))
    }

    /// [`Client::pc_identity_result`] with every error read as "did not answer", which is what
    /// makes "did not answer" and "is a different Prism Central" distinguishable for a caller
    /// that has already proved the connection some other way.
    pub async fn pc_identity(&self) -> Option<(String, Option<String>)> {
        self.pc_identity_result().await.ok().flatten()
    }

    /// Newest version first; the first version with a served answer pins the namespace. A
    /// 404 from every candidate means the version is not there, so the next one is tried; a
    /// 5xx or an undecodable answer means the service is down at that version, which is
    /// reported and not stepped past. Everything transient or host-wide - auth, connect,
    /// TLS, a certificate, a 429 that survived the retry, any other transport failure -
    /// aborts the whole negotiation instead of being pinned as a session-long verdict.
    async fn probe_namespace(&self, ns: &'static Namespace) -> Result<NamespaceStatus, PrismError> {
        let opts = ListOptions {
            limit: 1,
            ..Default::default()
        };
        let mut probed_any = false;
        // A version whose candidate list is empty issues no request at all - no listable kind
        // existed at that version yet - and the namespace is still named in the not-served
        // list below. Today every namespace has at least one candidate at every version.
        for &version in ns.versions {
            let mut down: Option<String> = None;
            for k in probe_candidates(ns, version) {
                probed_any = true;
                let path = with_version(k.list_path, version);
                match self.list_page_at(k, 0, &opts, Some(version)).await {
                    Ok(_) => {
                        return Ok(NamespaceStatus::served(
                            ns,
                            version,
                            format!("probed {path}"),
                        ));
                    }
                    // Served, this account just may not list this kind: a permission
                    // verdict, not an availability one.
                    Err(PrismError::Forbidden(_)) => {
                        return Ok(NamespaceStatus::served(
                            ns,
                            version,
                            format!("reachable ({path}: not permitted for this account)"),
                        ));
                    }
                    Err(PrismError::NotFound(_)) => continue,
                    // Any other 4xx (400 for a missing required parameter, 405, 412) proves
                    // the version is there.
                    Err(PrismError::Api {
                        status, message, ..
                    }) if status < 500 => {
                        return Ok(NamespaceStatus::served(
                            ns,
                            version,
                            format!("reachable ({path}: HTTP {status}: {message})"),
                        ));
                    }
                    Err(
                        e @ (PrismError::Auth
                        | PrismError::Connect { .. }
                        | PrismError::Tls(_)
                        | PrismError::Certificate { .. }
                        | PrismError::RateLimited { .. }
                        | PrismError::Transport(_)),
                    ) => return Err(e),
                    // Keep the first "down" reason: the shortest probe path is the most
                    // representative, and later candidates only repeat the outage. The path
                    // rides along, so the row says which endpoint was asked.
                    Err(e) => {
                        down.get_or_insert_with(|| down_detail(&path, &e));
                    }
                }
            }
            if let Some(detail) = down {
                return Ok(NamespaceStatus::down(ns, version, detail));
            }
        }
        let detail = if probed_any {
            format!("not served at {}", ns.versions.join(", "))
        } else {
            "no listable kind in catalog".to_string()
        };
        Ok(NamespaceStatus::not_served(ns, detail))
    }

    /// A poisoned routing table is taken as it stands, for the reason `buckets()` gives: the
    /// worst a panic under the guard can leave behind is one namespace's pin, and panicking a
    /// whole session's requests over it would be worse.
    fn pins_guard(&self) -> std::sync::RwLockReadGuard<'_, HashMap<&'static str, &'static str>> {
        self.pins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn statuses_guard(&self) -> std::sync::RwLockReadGuard<'_, Vec<NamespaceStatus>> {
        self.statuses
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether this namespace's routing came from a restore rather than from a probe: the pins
    /// [`Client::adopt_pins`] resolved, and the namespaces that restore said nothing about.
    /// Those are the only ones the lazy repair may re-probe, and the only ones an absent
    /// status row reads as *unknown* for rather than as not served.
    fn is_adopted(&self, namespace: &str) -> bool {
        self.adopted
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(namespace)
    }

    /// The budgets are shared state with no invariant worth protecting, so a poisoned lock is
    /// taken as it stands rather than panicking a whole session's requests: the worst a panic
    /// under the guard can leave behind is one bucket's counter.
    fn buckets(&self) -> std::sync::MutexGuard<'_, Buckets> {
        self.buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait for a token on this operation's budget and on the host ceiling. Never holds the
    /// lock across an await: it takes what it can, drops the guard, and sleeps outside it.
    ///
    /// The two takes are one step. Peeking first and spending only when both say yes keeps a
    /// caller parked on a tight tier from burning a host token on every attempt it never uses,
    /// which would drain the shared ceiling faster than the traffic that reaches the wire.
    async fn acquire(&self, method: &HttpMethod, template: &'static str, limit: RateLimit) -> bool {
        let mut slept = false;
        loop {
            let wait = {
                let mut guard = self.buckets();
                let buckets = &mut *guard;
                let now = std::time::Instant::now();
                let op = buckets
                    .per_op
                    .entry((method.clone(), template))
                    .or_insert_with(|| Bucket::new(limit));
                // The longer of the two waits, or `None` when neither refuses.
                match buckets.host.peek(now).max(op.peek(now)) {
                    None => {
                        buckets.host.take(now);
                        op.take(now);
                        None
                    }
                    some => some,
                }
            };
            let Some(w) = wait else { return slept };
            slept = true;
            // The reactive half logs its 429 waits; a caller frozen by the preventive half must
            // not be the one silent stall in a trace - nor the one invisible one on screen.
            self.metrics.record_throttled(std::time::Instant::now());
            if w >= Duration::from_secs(1) {
                tracing::warn!(%method, template, ?w, "paced by the local rate budget");
            } else {
                tracing::debug!(%method, template, ?w, "paced by the local rate budget");
            }
            tokio::time::sleep(w).await;
        }
    }

    /// A 429 arrived: hold this operation's budget and the host ceiling empty for what the
    /// server asked, so concurrent callers do not pile into the same wall.
    ///
    /// The retry `send` makes itself deliberately bypasses `acquire`: the server named the wait
    /// and `send` has already slept it, so pacing that one request again would only lengthen
    /// the stall it is recovering from.
    fn drain_bucket(&self, method: &HttpMethod, template: &'static str, retry_after: Duration) {
        let now = std::time::Instant::now();
        self.metrics.record_rate_limited(now);
        let mut guard = self.buckets();
        let buckets = &mut *guard;
        buckets.host.drain(now, retry_after);
        if let Some(b) = buckets.per_op.get_mut(&(method.clone(), template)) {
            b.drain(now, retry_after);
        }
    }

    /// The debt on one operation's budget in the current window, and the only way a test can
    /// see the pacing without waiting for it. `method` is an HTTP method name, `template` a
    /// catalog path template.
    ///
    /// Public only because `tests/actions.rs` is a separate crate; not a supported accessor.
    #[doc(hidden)]
    pub fn bucket_debt(&self, method: &str, template: &str) -> u32 {
        // A find, not a lookup: the map is keyed by `&'static str` and a test's template is not.
        self.buckets()
            .per_op
            .iter()
            .find(|((m, t), _)| m.as_str() == method && *t == template)
            .map_or(0, |(_, b)| b.debt())
    }

    /// The tier in force for one operation: the catalog's, or the tighter one that operation's
    /// own answers advertised. See `bucket_debt` for why this is a find rather than a lookup.
    #[doc(hidden)]
    pub fn bucket_limit(&self, method: &str, template: &str) -> Option<RateLimit> {
        self.buckets()
            .per_op
            .iter()
            .find(|((m, t), _)| m.as_str() == method && *t == template)
            .map(|(_, b)| b.limit())
    }

    /// The debt on the host ceiling in the current window; see `bucket_debt`.
    #[doc(hidden)]
    pub fn host_debt(&self) -> u32 {
        self.buckets().host.debt()
    }

    /// The host tier in force: the conservative start, or what a response advertised.
    ///
    /// Public only so a test can read the pacing without waiting for it; not a supported
    /// accessor.
    #[doc(hidden)]
    pub fn host_limit(&self) -> RateLimit {
        self.buckets().host.limit()
    }

    /// Whether this Prism Central has refused the credential this client holds.
    ///
    /// Once true it stays true, and every [`Client::send`] fails with [`PrismError::Auth`]
    /// without a request. The pollers that are not subscriptions - the stats counters, the name
    /// warm-up, the Disaster Recovery sampler - read it to stop their loops outright, so the
    /// session is not merely harmless but visibly finished.
    pub fn auth_rejected(&self) -> bool {
        self.auth.state() == AuthState::Rejected || !self.valve.open()
    }

    /// How many requests this client puts on the wire at once: never more than the host tier
    /// allows in one refresh period, and never more than [`MAX_FAN_OUT`].
    ///
    /// The startup burst is what this exists for. Negotiation fires a probe per namespace, and
    /// a fan-out of four against a Prism Central that allows three a second is over the limit
    /// before the first answer has come back. The
    /// bucket would pace them anyway; capping the fan-out as well means the burst is never
    /// *built*, so nothing queues behind a sleep that a shorter queue would have avoided.
    #[doc(hidden)]
    pub fn fan_out(&self) -> usize {
        let per_period = usize::try_from(self.host_limit().count).unwrap_or(MAX_FAN_OUT);
        per_period.clamp(1, MAX_FAN_OUT)
    }

    /// Adopt what one response said about the rate limits. Called for **every** answer, a 404
    /// and a 429 included: the headers ride on all of them, and an answer this client got wrong
    /// costs the Prism Central exactly as much as one it got right.
    ///
    /// A response that advertises nothing leaves the tier exactly as it was - see
    /// [`Advertised::ceiling`] - so this is safe to call unconditionally and safe against a
    /// Prism Central that never sends the headers at all.
    fn observe_limits(&self, headers: &HeaderMap, method: &HttpMethod, template: &'static str) {
        let advertised = Advertised::read(headers);
        let now = std::time::Instant::now();
        let mut guard = self.buckets();
        let before = guard.host.limit();
        let after = advertised.ceiling(before);
        if after != before {
            if after.tighter_than(before) {
                tracing::info!(
                    count = after.count,
                    per_secs = after.per_secs,
                    "the Prism Central advertises a tighter rate limit; slowing to it"
                );
            } else {
                tracing::debug!(
                    count = after.count,
                    per_secs = after.per_secs,
                    "adopting the advertised rate limit"
                );
            }
            guard.host.relimit(after);
        }
        // And now the same question for this one operation. A Prism Central states a tier per
        // endpoint - pc.7.6 states ten different ones, from two a second to thirty - and the
        // host bucket holds one number, whichever answer spoke last. So a `30/s` endpoint's
        // answer widens the bucket that a `3/s` endpoint's requests then pass through, and the
        // lab saw exactly that: `protected-resources` reached four a second against the three
        // it had advertised. Only a per-operation budget can hold an endpoint to what that
        // endpoint said.
        //
        // Tightening only, and this is the half that matters. The catalog's rate is what this
        // client declares for the operation and a header may lower it, never raise it. Widening
        // from the wire is how a client ends up running over a gateway's limit for a whole
        // session, which is the likeliest explanation the investigation found for the 401s, the
        // intermittent 503s and the two locked-out admin accounts.
        if let Some(stated) = advertised.tier
            && let Some(op) = guard.per_op.get_mut(&(method.clone(), template))
            && stated.tighter_than(op.limit())
        {
            tracing::info!(
                %method,
                template,
                count = stated.count,
                per_secs = stated.per_secs,
                "this endpoint advertises a tighter rate limit than the catalog; slowing to it"
            );
            op.relimit(stated);
        }
        // The longer budget is spent: hold before the request that would have earned the 429,
        // rather than after it. `drain` is the same hold a served 429 sets, so the two cannot
        // disagree about what "wait" means.
        if let Some(hold) = advertised.hold(after) {
            tracing::warn!(?hold, "the request budget is spent; holding");
            guard.host.drain(now, hold);
        }
    }

    /// One request, through the auth gate and the session, with one renewal if the session it
    /// was riding had ended.
    ///
    /// The asymmetry the whole design rests on is the `match` at the bottom. A 401 on a request
    /// that **carried the credential** is the credential being refused: terminal, never retried.
    /// A 401 on a request that carried only a **session cookie** is that session having ended:
    /// the jar is dropped and the request comes back round through the gate, so `N` requests
    /// holding the same stale cookie produce one re-authentication between them.
    async fn send(
        &self,
        method: &HttpMethod,
        template: &'static str,
        limit: RateLimit,
        req: reqwest::RequestBuilder,
    ) -> Result<(StatusCode, HeaderMap, Vec<u8>), PrismError> {
        for renewal in 0..=RENEWALS {
            let pass = match renewal {
                0 => Pass::First,
                n if n == RENEWALS => Pass::Authenticate,
                _ => Pass::Renewing,
            };
            // Every attempt is a request on the wire and is paced like any other. The 429 retry
            // inside `attempt` is the one thing that skips the budget, because the server named
            // the wait and `attempt` has already slept it.
            let paced = self.acquire(method, template, limit).await;
            match self.attempt(template, &req, pass, paced).await? {
                Attempt::Answered(answer) => return Ok(answer),
                Attempt::SessionEnded => continue,
            }
        }
        // Unreachable while `Pass::Authenticate` is the last go: a request that carried the
        // credential never reports `SessionEnded`. Here because the loop cannot prove it.
        Err(PrismError::Auth)
    }

    /// What this request will carry, and the permit it holds while it carries it.
    ///
    /// One rule, and the whole of the single-flight rests on it: **a credential goes out only
    /// while this request holds the permit**, and the request holding the permit presents
    /// exactly when the jar has no session to ride. Both halves are read behind the same
    /// permit, so one emptying of the jar buys one presentation however many requests are in
    /// flight - the semaphore does the excluding, and nothing is decided on a value another
    /// task may be in the middle of changing.
    ///
    /// The jar is the authority on whether there is a session, not [`AuthState`]. That is what
    /// makes the arithmetic exact: a session is emptied once, by whichever refusal owns its
    /// generation, and refilled by the presentation that follows before the permit is let go.
    ///
    /// The single exception is the degraded mode. A Prism Central that hands out nothing to
    /// reuse would send every request through the gate, one at a time, for the life of the
    /// session; [`GIVE_UP_RENEWALS`] settles that after two, and from then on every request
    /// carries the credential and none of them waits - which is exactly what this client did
    /// before session reuse existed.
    async fn carry(
        &self,
        pass: Pass,
    ) -> Result<(Carry, Option<tokio::sync::SemaphorePermit<'_>>), PrismError> {
        if !self.jar.usable() {
            return Ok((Carry::Credential, None));
        }
        match (self.auth.state(), pass) {
            // This Prism Central has refused this credential, and the one thing that must never
            // happen next is asking it again.
            (AuthState::Rejected, _) => return Err(PrismError::Auth),
            // The common path, and the only one that skips the gate: a credential the far end
            // has answered, and a session to ride. Nothing here needs excluding.
            (AuthState::Established, Pass::First) => {
                if let Some((generation, cookie)) = self.jar.ride() {
                    return Ok((Carry::Session(generation, cookie), None));
                }
            }
            _ => {}
        }
        let permit = self
            .auth
            .gate
            .acquire()
            .await
            .map_err(|_| PrismError::Auth)?;
        // Behind the permit: whoever held it before this request has finished, so a session it
        // opened is one this request can ride, and a jar that is still empty is this request's
        // to fill.
        if self.auth.state() == AuthState::Rejected {
            return Err(PrismError::Auth);
        }
        match (pass, self.jar.ride()) {
            // Dropping the permit here is what keeps the client concurrent: the gate is for
            // authenticating, not for the session it opens.
            (Pass::First | Pass::Renewing, Some((generation, cookie))) => {
                Ok((Carry::Session(generation, cookie), None))
            }
            _ => Ok((Carry::Credential, Some(permit))),
        }
    }

    async fn attempt(
        &self,
        template: &'static str,
        original: &reqwest::RequestBuilder,
        pass: Pass,
        paced: bool,
    ) -> Result<Attempt, PrismError> {
        // The valve, before the state machine and before anything else. It is the one check
        // here that does not depend on a conclusion this crate drew: see `crate::valve`.
        if !self.valve.open() {
            refused_before_the_network(template);
            self.metrics.record_call(refused_call(template));
            return Err(PrismError::Auth);
        }
        let (carry, permit) = match self.carry(pass).await {
            Ok(pair) => pair,
            Err(e) => {
                refused_before_the_network(template);
                self.metrics.record_call(refused_call(template));
                return Err(e);
            }
        };
        let req = original
            .try_clone()
            .ok_or_else(|| PrismError::Transport("request body cannot be replayed".into()))?;
        // The session travels **with the request**. Leaving it to the cookie layer to look up
        // again at dispatch is what let a request go out carrying neither a session nor a
        // credential: the jar can be emptied by another task between the decision and the send.
        //
        // `riding` is the generation of the session on this request, and `None` says it carries
        // the credential instead. The two are the same fact read from opposite ends, which is
        // why they are one binding rather than two that could disagree.
        let (riding, req) = match carry {
            Carry::Credential => (None, req.basic_auth(&self.username, Some(&self.password))),
            Carry::Session(generation, cookie) => {
                (Some(generation), req.header(header::COOKIE, cookie))
            }
        };
        let presented = riding.is_none();
        debug_assert!(
            presented || pass != Pass::Authenticate,
            "the last go authenticates"
        );
        let req = req
            .header(header::ACCEPT, "application/json")
            .build()
            .map_err(|e| PrismError::Transport(describe(&e)))?;
        // The key `acquire` spent from, so the drain below empties the budget this request
        // actually paced against.
        let method = req.method().clone();
        // For the log, beside the metrics, and off the built request rather than the template:
        // the template says what was meant and the URL says what went out, and a person reading
        // a log after the fact needs the second.
        let url = safe_url(req.url());
        // The last gate, and the only one that sees where this request is actually going. The
        // session travels as a header `attempt` set itself, so neither the redirect policy nor
        // the jar's own origin rule was consulted for it: `ride` hands over a `Cookie` value
        // and knows nothing about the URL it will ride on.
        if !may_carry(&self.jar.origin, req.url(), presented) {
            return Err(PrismError::Transport(format!(
                "refusing to send this Prism Central's session to {url}"
            )));
        }
        let carried = if presented {
            Carried::Presented
        } else {
            Carried::Session
        };
        let retry = req.try_clone();
        // Before `execute`, so a request that never comes back is still counted as having gone
        // out. `send` is the one funnel - the post-429 retry and `act`'s pre-action ETag read
        // included - which is the whole reason to count here rather than at the call sites.
        let mut began = std::time::Instant::now();
        self.metrics.record_started(began);
        let first = match self.http.execute(req).await {
            Ok(r) => r,
            Err(e) => {
                self.metrics.record_failed(std::time::Instant::now());
                let e = self.map_transport(e);
                self.trace(&method, &url, carried, began, paced, Err(&e));
                return Err(e);
            }
        };
        // Before anything is decided about the answer: the pacing headers ride on every one,
        // and a response whose status sends us down the retry path below has already told us
        // what rate to come back at.
        self.observe_limits(first.headers(), &method, template);
        let resp = match (first.status(), retry) {
            (StatusCode::TOO_MANY_REQUESTS, Some(retry)) => {
                let wait = retry_after(first.headers());
                tracing::warn!(?wait, "rate limited, retrying once");
                self.drain_bucket(&method, template, wait);
                tokio::time::sleep(wait).await;
                // The clock restarts on the retry: the elapsed below measures the request that
                // actually produced `resp`, and the `Retry-After` sleep stays where it belongs,
                // in `throttled`. Folding a wait the server named - five seconds by default,
                // sixty at the cap - into a mean of tens of milliseconds would stop it being a
                // latency after the first rate limit.
                began = std::time::Instant::now();
                // It cost the Prism Central twice, so it counts twice.
                self.metrics.record_started(began);
                match self.http.execute(retry).await {
                    Ok(r) => {
                        self.observe_limits(r.headers(), &method, template);
                        r
                    }
                    Err(e) => {
                        self.metrics.record_failed(std::time::Instant::now());
                        let e = self.map_transport(e);
                        self.trace(&method, &url, carried, began, paced, Err(&e));
                        return Err(e);
                    }
                }
            }
            _ => first,
        };
        // A 401 on a request that rode a session is that session having ended, not a refused
        // password: drop the jar and let the caller come back round through the gate. The
        // generation is what makes that exactly one renewal - the jar is emptied once, and the
        // one request that then finds it empty behind the permit fills it for everybody else.
        if let (StatusCode::UNAUTHORIZED, Some(generation)) = (resp.status(), riding) {
            drop(permit);
            self.metrics.record_failed(std::time::Instant::now());
            self.trace(&method, &url, carried, began, paced, Ok(resp.status()));
            self.jar.expire(generation);
            return Ok(Attempt::SessionEnded);
        }
        if !presented {
            self.jar.answered();
        }
        // A credential the far end refused. The valve latches on its own count as well as on
        // this one verdict, so a bug in `settle` cannot turn one refusal into a hundred.
        if presented && resp.status() == StatusCode::UNAUTHORIZED {
            self.valve.refused(std::time::Instant::now());
            self.valve.latch();
        }
        // A presentation the far end answered. Whether it opened a session is how this client
        // tells a Prism Central worth riding from one that hands out nothing: see
        // [`SessionJar::presented`].
        if presented && resp.status() != StatusCode::UNAUTHORIZED {
            self.jar.presented();
        }
        // What this answer proved about the credential: see [`Auth::settle`]. Held here rather
        // than at the call sites because `send` is the one funnel - the post-429 retry and
        // `act`'s pre-action ETag read included.
        self.auth.settle(presented, resp.status());
        drop(permit);
        // `failed` is the pipe, not the payload. A 404 or a 403 is a *result* and colours
        // nothing; a 401 and a 5xx say the far end is not answering for reasons no view can
        // report as a row.
        if resp.status() == StatusCode::UNAUTHORIZED || resp.status().is_server_error() {
            self.metrics.record_failed(std::time::Instant::now());
        }
        // A 429 that survived - the retry hit the same wall, or the request could not be cloned
        // and there was no retry. The hold set before the retry has expired by definition, so
        // without this the next caller walks straight into the wall this one just found.
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            self.drain_bucket(&method, template, retry_after(resp.headers()));
        }
        let status = resp.status();
        let headers = resp.headers().clone();
        // `execute` resolves at the *headers*, so the body is still on the wire here. Counting
        // the request completed before this read would call a connection that dies mid-body a
        // success - the transport failure the spec's §9.1 counts red - and would leave a
        // 500-row page's transfer out of the elapsed the meter reports as a round trip.
        let body = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                self.metrics.record_failed(std::time::Instant::now());
                let e = PrismError::Transport(describe(&e));
                self.trace(&method, &url, carried, began, paced, Err(&e));
                return Err(e);
            }
        };
        self.metrics
            .record_completed(std::time::Instant::now().saturating_duration_since(began));
        self.trace(&method, &url, carried, began, paced, Ok(status));
        Ok(Attempt::Answered((status, headers, body.to_vec())))
    }

    /// 412 stays an `Api` error here: only the caller knows which entity and action it meant,
    /// so `act` intercepts that status before this runs. `path` stands in for the message when
    /// the body carries none: a bare "HTTP 404" would only repeat what the variant prints.
    fn ok(
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
        path: &str,
    ) -> Result<envelope::Envelope, PrismError> {
        let message = || envelope::error_message(body).unwrap_or_else(|| path.to_string());
        match status.as_u16() {
            200..=299 => envelope::parse(body),
            401 => Err(PrismError::Auth),
            403 => Err(PrismError::Forbidden(message())),
            404 => Err(PrismError::NotFound(message())),
            429 => Err(PrismError::RateLimited {
                retry_after: retry_after(headers),
            }),
            // A 5xx is "not now", so the header that says how long is kept with it. `ok` is
            // the only place both the status and the headers are in hand.
            s => Err(PrismError::Api {
                status: s,
                message: message(),
                retry_after: headers
                    .contains_key(header::RETRY_AFTER)
                    .then(|| retry_after(headers)),
            }),
        }
    }

    /// Keep the whole source chain: reqwest's Display hides "invalid peer certificate: UnknownIssuer".
    fn map_transport(&self, e: reqwest::Error) -> PrismError {
        if let Some(reason) = certificate_error(&e) {
            return PrismError::Certificate {
                host: self.host.clone(),
                reason,
            };
        }
        if e.is_connect() || e.is_timeout() {
            PrismError::Connect {
                host: self.host.clone(),
                message: describe(&e),
            }
        } else {
            PrismError::Transport(describe(&e))
        }
    }
}

/// The one error both ETag paths raise: no validator, so no conditional request to build.
fn missing_etag(kind: &Kind, ext_id: &str) -> PrismError {
    PrismError::Decode(format!("{} {ext_id} returned no ETag header", kind.id))
}

/// `data` read as the task reference an accepted mutation returns. A v4 answer that names itself
/// settles it, because every *entity* carries `extId` too: an action whose 200 hands back the
/// updated entity must not mint a task the watcher would then poll for a task that never existed.
/// An answer with no `$objectType` falls back to the bare `extId` rule.
fn task_ref(data: &Value) -> Option<TaskRef> {
    let names_a_task = match data.get("$objectType").and_then(Value::as_str) {
        Some(ty) => ty.ends_with("TaskReference"),
        None => true,
    };
    if !names_a_task {
        return None;
    }
    data.get("extId")
        .and_then(Value::as_str)
        .map(|id| TaskRef { ext_id: id.into() })
}

/// The catalog's guess, degraded by what actually arrived. 202 is still not required: the
/// client trusts the envelope, not the status.
fn result_of(returns: ActionReturn, data: Value, kind: &str, action: &str) -> ActionResult {
    let as_task = task_ref(&data);
    match (returns, as_task) {
        (ActionReturn::Task, Some(t)) => ActionResult::Task(t),
        (ActionReturn::Task, None) if data.is_null() => ActionResult::Empty,
        (ActionReturn::Task, None) => {
            tracing::debug!(kind, action, "catalog said Task, answer carried a document");
            ActionResult::Payload(data)
        }
        (ActionReturn::Payload | ActionReturn::None, _) if data.is_null() => ActionResult::Empty,
        (ActionReturn::None, _) => {
            tracing::debug!(kind, action, "catalog said None, answer carried a document");
            ActionResult::Payload(data)
        }
        (ActionReturn::Payload, _) => ActionResult::Payload(data),
    }
}

/// `kind`'s path with `parents` (and `tail`, the entity for a get path) filled in. A count
/// mismatch is a programming error, reported as `Catalog` naming the kind, the template, and
/// both counts.
fn scoped_path(
    kind: &Kind,
    template: &str,
    parents: &[&str],
    tail: Option<&str>,
) -> Result<String, PrismError> {
    let mut values: Vec<&str> = parents.to_vec();
    values.extend(tail);
    fill_placeholders(template, &values).ok_or_else(|| {
        PrismError::Catalog(format!(
            "{}: {template} needs {} path value(s), got {}",
            kind.id,
            placeholder_count(template),
            values.len()
        ))
    })
}

/// The kinds worth one `$limit=1` list to learn whether `ns` serves `version`: top-level,
/// pageable, parameter-free, known to exist at that version, shortest paths first. More than
/// one, because a served namespace can still lack one kind (multidomain serves
/// `management/local-domain` and 404s `config/locations`).
/// The detail for a namespace that is down at a version: the probed path, then the error.
/// An `Api` error whose body carried no message already names the path (`HTTP 503: /x`), so
/// it is not prefixed a second time.
fn down_detail(path: &str, e: &PrismError) -> String {
    let text = e.to_string();
    if text.contains(path) {
        text
    } else {
        format!("{path}: {text}")
    }
}

fn probe_candidates(ns: &Namespace, version: &str) -> Vec<&'static Kind> {
    let mut candidates: Vec<&'static Kind> = KINDS
        .iter()
        .filter(|k| {
            k.namespace == ns.name
                && k.is_top_level()
                && k.list_params.limit
                && !k.list_params.required
                && k.served_at(version)
        })
        .collect();
    candidates.sort_by_key(|k| (k.list_path.len(), k.list_path));
    candidates.truncate(PROBE_CANDIDATES);
    candidates
}

/// Candidates as `name (extId)`, for an error that has to tell the user what to pick from.
/// An empty list reads `(none)` rather than trailing off after the colon.
/// Not named `describe`: that is the error-chain helper this module already imports.
fn name_and_id<'a>(cs: impl IntoIterator<Item = &'a Entity>) -> String {
    let listed = cs
        .into_iter()
        .map(|c| format!("{} ({})", c.name, c.ext_id))
        .collect::<Vec<_>>()
        .join(", ");
    if listed.is_empty() {
        "(none)".to_string()
    } else {
        listed
    }
}

/// Pages of `limit` items needed to cover `total`, so page indices run `0..page_count`.
/// Saturating, not truncating. The `as u32` this replaces wrapped a total too large to walk
/// down to a small page count, which is exactly the number that would slip past `list_all`'s
/// ceiling: 2^63-1 rows at a hundred a page came out as 2,061,584,303 pages, and a total one
/// page wider would have come out as a handful.
fn page_count(total: u64, limit: u64) -> u32 {
    u32::try_from(total.div_ceil(limit.max(1))).unwrap_or(u32::MAX)
}

/// What a request carried, which is the field that answers "which call was it that failed the
/// credentials". A 401 means two different things depending on this word, and the asymmetry
/// `Client::send` rests on is exactly that difference: `Presented` and 401 is a credential the
/// far end refused, terminal and never retried; `Session` and 401 is a session that had ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carried {
    /// It presented the credential.
    Presented,
    /// It rode a session cookie the credential had already opened.
    Session,
    /// It never went out: this client had already been refused, and the one thing that must
    /// never happen next is asking again.
    Refused,
}

impl Carried {
    fn word(self) -> &'static str {
        match self {
            Carried::Presented => "presented",
            Carried::Session => "session",
            Carried::Refused => "refused",
        }
    }
}

/// One event per request, from the one funnel every request goes through - the post-429 retry
/// and `act`'s pre-action ETag read included.
///
/// **What it leaves out is the point of it.** The method, the URL, the status, the round trip
/// and the word for what the request carried; never a header, never a cookie, never a body. The
/// `Authorization` header and the session cookies Prism Central issues are the two things this
/// event sits closest to, and `Carried` is how it says which of them went out without saying
/// what either of them was. `nutsh_core::log` argues the whole rule and `tests/log.rs` proves
/// it against a live session.
///
/// A 401 or a 5xx is a `warn` and everything else a `debug`, in the shape `acquire` already
/// uses for its two pacing lines: a person who turned logging on because a session stopped
/// working should not have to read at `debug` to find the answer.
fn trace_request(
    method: &reqwest::Method,
    url: &str,
    carried: Carried,
    began: std::time::Instant,
    outcome: Result<StatusCode, &PrismError>,
) {
    let ms = began.elapsed().as_millis();
    let credential = carried.word();
    match outcome {
        Ok(status) if status == StatusCode::UNAUTHORIZED || status.is_server_error() => {
            tracing::warn!(%method, %url, status = status.as_u16(), ms, %credential, "request");
        }
        Ok(status) => {
            tracing::debug!(%method, %url, status = status.as_u16(), ms, %credential, "request");
        }
        Err(e) => tracing::warn!(%method, %url, ms, %credential, error = %e, "request failed"),
    }
}

/// A request that never reached the network, because this Prism Central has already refused the
/// credential this client holds. There is no method, no status and no round trip to report, and
/// the template is the only thing there is to name - but the line has to exist: a poller that
/// has gone quiet must not be the one silent stall in a log.
fn refused_before_the_network(template: &'static str) {
    tracing::warn!(
        %template,
        credential = %Carried::Refused.word(),
        "request refused before the network; this Prism Central rejected the credential"
    );
}

/// A URL with any userinfo blanked. No caller builds one - [`Profile`] has a host, a port and a
/// username, and the username goes in a header - but a URL is the one thing this event prints
/// verbatim, and the rule about credentials is not a rule that should depend on that staying
/// true.
/// The path and query of a URL, for the ring: `https://host:9440/api/x?y` is `/api/x?y`.
fn path_of(url: &str) -> String {
    url.find("://")
        .and_then(|i| url[i + 3..].find('/').map(|j| url[i + 3 + j..].to_string()))
        .unwrap_or_else(|| url.to_string())
}

/// A ring entry for a request this client refused to send.
fn refused_call(template: &'static str) -> crate::metrics::Call {
    crate::metrics::Call {
        at: std::time::Instant::now(),
        method: "-".into(),
        path: template.to_string(),
        status: None,
        ms: 0,
        credential: Carried::Refused.word(),
        paced: false,
    }
}

fn safe_url(url: &reqwest::Url) -> String {
    if url.username().is_empty() && url.password().is_none() {
        return url.to_string();
    }
    let mut clean = url.clone();
    let _ = clean.set_password(None);
    let _ = clean.set_username("");
    clean.to_string()
}

/// `Retry-After` as integer seconds, capped at 60 s. HTTP-date or garbage waits 5 s.
pub fn retry_after(headers: &HeaderMap) -> Duration {
    let secs = headers
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(5);
    Duration::from_secs(secs).min(MAX_RETRY_AFTER)
}

/// Whether a failed list is evidence that the **pin** is wrong, and so worth re-negotiating
/// the namespace for: the endpoint is not there at this version, or the service behind it is
/// down at this version.
///
/// A permission verdict (403) and a transport failure are about the account and the network,
/// not the version. Nor is a body that will not decode, which `probe_namespace` does count
/// against a candidate version: a probe is asking whether a version answers at all, while
/// this is a known endpoint answering with something this client could not read, and no
/// amount of re-negotiating makes a malformed payload well formed. `lifecycle.resources.config`
/// answered `expected a list, got object` on every poll, and every one of those re-negotiated
/// a whole namespace against a Prism Central that allows three requests a second.
fn worth_repairing(e: &PrismError) -> bool {
    matches!(
        e,
        PrismError::NotFound(_) | PrismError::Api { status: 500.., .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time guard for the boxing in `negotiate`: the TUI drives a whole connection
    /// through a `BoxFuture<'static, _>`, so every step of one has to stay `Send`. This
    /// function is never called; it fails to *compile* if that stops being true.
    fn _a_whole_connection_is_send(profile: Profile) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let client = Client::connect(&profile, "x").expect("build client");
            let _ = client.negotiate().await;
            let _ = client.pc_identity_result().await;
            let _ = client.resolve_cluster("x").await;
        })
    }

    /// The three words the `credential` field can take, which is what a person greps a log for
    /// when a session stops being accepted. `presented` and `session` ride every request and
    /// `tests/log.rs` pins both against a live 401; `refused` is a request this client stopped
    /// before the network, which only a session that is still polling after the valve latched
    /// can produce, so it is pinned here.
    #[test]
    fn a_request_says_which_of_three_ways_it_carried_the_credential() {
        assert_eq!(Carried::Presented.word(), "presented");
        assert_eq!(Carried::Session.word(), "session");
        assert_eq!(Carried::Refused.word(), "refused");
    }

    /// A URL is the one thing the request event prints verbatim, so userinfo in one is blanked
    /// rather than trusted not to exist.
    #[test]
    fn a_url_with_a_password_in_it_is_blanked_before_it_is_logged() {
        let plain = reqwest::Url::parse("https://pc.lab:9440/api/vmm/v4.1/ahv/config/vms").unwrap();
        assert_eq!(safe_url(&plain), plain.to_string());
        let dressed = reqwest::Url::parse("https://admin:hunter2@pc.lab:9440/api/x").unwrap();
        let safe = safe_url(&dressed);
        assert!(!safe.contains("hunter2"), "{safe}");
        assert!(safe.contains("pc.lab:9440/api/x"), "{safe}");
    }

    /// Every arm of `result_of`, including the degrade arms no mock action reaches: the point
    /// of the match is that a wrong `returns` degrades instead of erroring, and a mutation the
    /// server accepted must never read as a failure.
    #[test]
    fn result_of_degrades_when_the_catalog_guesses_wrong() {
        use serde_json::json;

        let task = |id: &str| ActionResult::Task(TaskRef { ext_id: id.into() });
        let of = |r, v| result_of(r, v, "test.config.Thing", "act");

        // The catalog guessed right.
        assert_eq!(
            of(ActionReturn::Task, json!({"extId": "t"})),
            task("t"),
            "no discriminator falls back to the bare extId rule"
        );
        assert_eq!(
            of(
                ActionReturn::Task,
                json!({"$objectType": "prism.v4.config.TaskReference", "extId": "t"})
            ),
            task("t")
        );
        assert_eq!(
            of(ActionReturn::Payload, json!({"message": "x"})),
            ActionResult::Payload(json!({"message": "x"}))
        );

        // It guessed wrong, and every wrong guess degrades to what arrived.
        assert_eq!(
            of(ActionReturn::Task, json!({"message": "x"})),
            ActionResult::Payload(json!({"message": "x"}))
        );
        assert_eq!(
            of(
                ActionReturn::Task,
                json!({"$objectType": "vmm.v4.ahv.config.Vm", "extId": "v"})
            ),
            ActionResult::Payload(json!({"$objectType": "vmm.v4.ahv.config.Vm", "extId": "v"})),
            "an entity carries extId too, and is not a task to watch"
        );
        assert_eq!(
            of(ActionReturn::None, json!({"message": "x"})),
            ActionResult::Payload(json!({"message": "x"}))
        );

        // A 204 or an empty envelope is `Empty` whatever the catalog said.
        for returns in [
            ActionReturn::Task,
            ActionReturn::Payload,
            ActionReturn::None,
        ] {
            assert_eq!(of(returns, Value::Null), ActionResult::Empty, "{returns:?}");
        }
    }

    #[test]
    fn down_detail_names_the_path_once() {
        let path = "/opsmgmt/v4.0/config/reports";
        let bare = PrismError::Api {
            status: 503,
            message: path.to_string(),
            retry_after: None,
        };
        assert_eq!(
            down_detail(path, &bare),
            "HTTP 503: /opsmgmt/v4.0/config/reports"
        );
        let with_message = PrismError::Api {
            status: 503,
            message: "service unavailable".to_string(),
            retry_after: None,
        };
        assert_eq!(
            down_detail(path, &with_message),
            "/opsmgmt/v4.0/config/reports: HTTP 503: service unavailable"
        );
        let decode = PrismError::Decode("expected value".to_string());
        assert_eq!(
            down_detail(path, &decode),
            "/opsmgmt/v4.0/config/reports: cannot decode response: expected value"
        );
    }

    #[test]
    fn page_count_covers_the_last_partial_page() {
        assert_eq!(page_count(3, 2), 2);
        assert_eq!(page_count(4, 2), 2);
        assert_eq!(page_count(5, 2), 3);
        assert_eq!(page_count(1, 100), 1);
        assert_eq!(page_count(0, 100), 0);
        // And a total no walk could reach saturates rather than wrapping, so `list_all`'s
        // ceiling sees a number that is too large instead of one that fits.
        assert_eq!(page_count(u64::MAX, 100), u32::MAX);
        assert_eq!(page_count(u64::from(u32::MAX) * 100 + 1, 100), u32::MAX);
    }

    #[test]
    fn name_and_id_lists_candidates_and_says_none_when_empty() {
        let kind = nutsh_catalog::kind("clustermgmt.config.Cluster").unwrap();
        let one = Entity::new(
            kind,
            serde_json::json!({"extId": "id-1", "name": "a"}),
            None,
        );
        let two = Entity::new(
            kind,
            serde_json::json!({"extId": "id-2", "name": "b"}),
            None,
        );
        assert_eq!(name_and_id(&[one, two][..]), "a (id-1), b (id-2)");
        assert_eq!(name_and_id(std::iter::empty::<&Entity>()), "(none)");
    }

    #[test]
    fn retry_after_is_parsed_and_capped() {
        let mut h = HeaderMap::new();
        h.insert(header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_after(&h), Duration::from_secs(2));
        h.insert(header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(retry_after(&h), Duration::from_secs(60));
        h.insert(
            header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&h), Duration::from_secs(5));
        assert_eq!(retry_after(&HeaderMap::new()), Duration::from_secs(5));
    }

    /// What the lazy repair is and is not for. A malformed body is the one that had to leave:
    /// `lifecycle.resources.config` answered `expected a list, got object` on every poll, and
    /// each of those re-negotiated the whole namespace against a Prism Central that allows
    /// three requests a second.
    #[test]
    fn only_a_wrong_pin_is_worth_re_negotiating() {
        assert!(worth_repairing(&PrismError::NotFound("/x".into())));
        assert!(worth_repairing(&PrismError::Api {
            status: 503,
            message: "down".into(),
            retry_after: None,
        }));
        assert!(
            !worth_repairing(&PrismError::Decode("expected a list, got object".into())),
            "a body that will not decode says nothing about the version"
        );
        assert!(!worth_repairing(&PrismError::Api {
            status: 403,
            message: "no".into(),
            retry_after: None,
        }));
        assert!(!worth_repairing(&PrismError::Transport("dns".into())));
    }

    fn lab() -> Profile {
        Profile {
            host: "pc.lab".into(),
            port: 9440,
            username: "admin".into(),
            verify_tls: true,
            ca_bundle: None,
            plain_http: false,
        }
    }

    fn jar_for(profile: &Profile) -> SessionJar {
        SessionJar {
            origin: Origin::of(profile),
            inner: std::sync::RwLock::new(Jarred::default()),
        }
    }

    /// The third reading of the origin, and the one the other two do not reach. `cookies` and
    /// `set_cookies` are reqwest's cookie layer asking. But the session does not travel by that
    /// layer: `attempt` sets the `Cookie` header itself, from `ride`, which hands over a header
    /// value and never sees a URL. Between them sat a request that nothing had asked where it
    /// was going.
    ///
    /// Nothing can reach it today - every URL comes from `raw_url`, and a cookie is only in the
    /// jar because `set_cookies` accepted it for this same origin - which is why it is pinned
    /// here rather than through the mock: the situation it refuses cannot be built out of the
    /// public API, and the day it can, this is what says no.
    #[test]
    fn a_session_rides_only_to_the_origin_its_jar_came_from() {
        let origin = Origin::of(&lab());
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(
            may_carry(
                &origin,
                &url("https://pc.lab:9440/api/vmm/v4.3/ahv/config/vms"),
                false
            ),
            "its own Prism Central, which is every request this client builds"
        );
        for elsewhere in [
            "https://attacker.example:9440/api/vmm/v4.3/ahv/config/vms",
            // The suffix, which a `starts_with`/`ends_with` origin test would wave through.
            "https://pc.lab.attacker.example:9440/api/vmm/v4.3/ahv/config/vms",
            "https://pc.lab:9441/api/vmm/v4.3/ahv/config/vms",
            "http://pc.lab:9440/api/vmm/v4.3/ahv/config/vms",
        ] {
            assert!(!may_carry(&origin, &url(elsewhere), false), "{elsewhere}");
            // A request carrying the credential is not this function's to judge: the auth gate
            // and the valve decided that, and a second opinion here would be a second place
            // for them to disagree.
            assert!(may_carry(&origin, &url(elsewhere), true), "{elsewhere}");
        }
    }

    /// The jar answers for one origin and is deaf to every other, which is the half of the
    /// redirect fix that survives the policy being relaxed.
    ///
    /// Without it, `cookies` would hand the session to whatever host asked, and `set_cookies`
    /// would bank a `Set-Cookie` from any host and replay it to the configured Prism Central on
    /// the next request.
    #[test]
    fn the_jar_speaks_only_to_the_prism_central_it_was_built_for() {
        use reqwest::cookie::CookieStore;

        let jar = jar_for(&lab());
        let mine = Url::parse("https://pc.lab:9440/api/vmm/v4.3/ahv/config/vms").unwrap();
        let theirs = Url::parse("http://attacker.example/api/vmm/v4.3/ahv/config/vms").unwrap();
        let session = header::HeaderValue::from_static("NTNX_IAM_SESSION=s3cret; Path=/");

        // Inbound: a `Set-Cookie` from anywhere else is not banked, so it can never be sent
        // back to the real host.
        jar.set_cookies(&mut [&session].into_iter(), &theirs);
        assert_eq!(jar.cookies(&mine), None, "nothing was banked");

        // Inbound from the Prism Central itself is the ordinary path, and still works.
        jar.set_cookies(&mut [&session].into_iter(), &mine);
        assert_eq!(
            jar.cookies(&mine).map(|v| v.to_str().unwrap().to_string()),
            Some("NTNX_IAM_SESSION=s3cret".to_string())
        );

        // Outbound: and now that there *is* a session, no other host is offered it.
        assert_eq!(jar.cookies(&theirs), None, "not to another host");
        assert_eq!(
            jar.cookies(&Url::parse("http://pc.lab:9440/api/x").unwrap()),
            None,
            "nor to the same name over plain HTTP"
        );
        assert_eq!(
            jar.cookies(&Url::parse("https://pc.lab:9441/api/x").unwrap()),
            None,
            "nor to another port on it"
        );
        assert_eq!(
            jar.cookies(&Url::parse("https://pc.lab.attacker.example:9440/api/x").unwrap()),
            None,
            "nor to a name this one is a prefix of"
        );
    }

    /// The port an https URL leaves out is still the port: `Url` drops `:443`, and a jar that
    /// compared `port()` would refuse its own Prism Central.
    #[test]
    fn the_default_port_is_the_port_the_profile_spells_out() {
        let origin = Origin::of(&Profile { port: 443, ..lab() });
        assert!(origin.is(&Url::parse("https://pc.lab/api/x").unwrap()));
        assert!(origin.is(&Url::parse("https://PC.LAB:443/api/x").unwrap()));
    }
}
