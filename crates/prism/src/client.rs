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

mod negotiate;
mod pacing;
mod requests;
mod session;

use pacing::*;
use requests::*;
use session::*;

pub const PAGE_LIMIT_MAX: u32 = 100;

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

/// What a table or a pane says instead of rows when its list answered 404: the request was
/// made and this is what came back. Here rather than in the TUI because
/// [`Availability::ListNotFound`] has to read the same words without a second request.
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
            // Both versions: the catalog first saw this path at one, this session negotiated the
            // other, and naming either alone reads as a promise nothing here can make. `:try` is
            // how a reader makes the server answer for itself.
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
    /// Per namespace, the version negotiation settled on, or the pins a restore adopted. Behind an
    /// `RwLock` because `adopt_pins`, the lazy repair and the `$select` give-up write after the
    /// client is in an `Arc`. Never held across an await.
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
    /// Kinds whose top-level list answered 404, by kind id. The scheduler decides which 404 counts
    /// (a child list's is a gone parent) and calls [`Client::mark_missing`]; a completing cycle
    /// calls [`Client::forget_missing`], so `^r` is a way back. Here and not only in the store
    /// because the store's flag is per table, and a kind is also a pane, a menu row and a counter.
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

/// `Retry-After` as integer seconds, capped at 60 s. HTTP-date or garbage waits 5 s.
pub fn retry_after(headers: &HeaderMap) -> Duration {
    let secs = headers
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(5);
    Duration::from_secs(secs).min(MAX_RETRY_AFTER)
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

    /// The request meter. Cheap to call every frame: it clones nothing and computing a
    /// [`crate::Meter`] out of it is `Metrics::snapshot`.
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::negotiate::*;
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

    /// The third reading of the origin: `attempt` sets the `Cookie` header itself and never sees
    /// a URL, so `may_carry` is the check against the built request. Unreachable through the
    /// public API today, which is why it is pinned here and not through the mock.
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

    /// The jar answers for one origin and is deaf to every other: without this `cookies` would
    /// hand the session to whatever host asked, and `set_cookies` would bank any host's cookie.
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
