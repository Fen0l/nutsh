//! In-process mock Prism Central that replays fixtures under the real API paths.
//!
//! Behaviour the client tests rely on: Basic auth, `$page`/`$limit` paging with
//! `totalAvailableResults` (an out-of-range `$limit` is a 400, not a clamp), `ETag` on
//! single-entity GETs, `NTNX-Request-Id` required on every mutation, `If-Match` required
//! (400) and checked (412) on the entity mutations the catalog marks `needs_etag` and refused
//! (400) on the ones it does not, a body rejected (400) on `$actions` the catalog marks
//! `takes_body: false` and required (400) where `needs_body` is true, 202 with a task
//! reference, empty lists for catalog paths without fixtures, and opt-in 404 and 403
//! namespaces and one-shot 429s, and per-namespace version lists that map older versions onto
//! the fixtures, and opt-in namespaces that fail with a chosen status and single paths that
//! are missing from, or failing inside, an otherwise served namespace.
//!
//! Mutations change the store: an accepted one inserts a task that advances one step per GET by
//! extId, and applies its effect on the transition into `SUCCEEDED`. Hooks make a task fail or
//! stall, forbid one kind's action (403), change an entity between the ETag fetch and the
//! mutation that follows (412), hold a path's answers behind a gate the test opens by hand, or
//! repeat a fixture up to N rows with fresh extIds.

mod handler;
mod store;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;

pub use store::Store;

#[derive(Debug, Clone, PartialEq)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    /// Lower-case header names.
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    /// When the mock received it. What a test asserting a *rate* measures against: the client's
    /// own meter is a ten-second average and cannot say whether four requests shared one second.
    pub at: std::time::Instant,
}

impl RecordedRequest {
    /// Header lookup by name, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Whether the request presented a credential, as opposed to riding a session cookie. The
    /// one thing a lockout test counts.
    pub fn authenticates(&self) -> bool {
        self.header("authorization").is_some()
    }
}

/// A gate a test holds a path's answers behind: the handler awaits it before it answers, and
/// [`Gate::release`] lets that many answers through.
///
/// Deterministic on purpose: no sleeps, and no assertion against a wall clock. A test that wants
/// a table mid-walk holds the path, draws the frame it means, and releases exactly the pages it
/// wants to land.
///
/// A semaphore starting at zero permits is exactly this, FIFO wakeups included: `release(n)` adds
/// permits, and each answer takes one and forgets it, so a released permit is spent rather than
/// returned when the answer is written.
#[derive(Clone, Debug)]
pub struct Gate(Arc<tokio::sync::Semaphore>);

impl Gate {
    /// A shut gate: nothing passes until [`Gate::release`].
    pub fn new() -> Gate {
        Gate(Arc::new(tokio::sync::Semaphore::new(0)))
    }

    /// Let `n` more answers through.
    pub fn release(&self, n: usize) {
        self.0.add_permits(n);
    }

    /// Wait for one permit and take it.
    async fn acquire(&self) {
        self.0
            .acquire()
            .await
            .expect("the gate is never closed")
            .forget();
    }
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::new()
    }
}

pub struct Shared {
    pub(crate) store: RwLock<Store>,
    /// Where the fixture tree keeps tasks: the catalog's Task list path at the version the
    /// prism fixtures carry, so a tree recorded from another Prism Central release still
    /// advances the tasks the mock creates.
    pub(crate) tasks_path: String,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) unavailable: Vec<String>,
    pub(crate) forbidden: Vec<String>,
    pub(crate) failing: HashMap<String, u16>,
    pub(crate) missing: Vec<String>,
    /// Behind a `Mutex` because [`MockPc::fail_from_now`] arms one on the running mock: a test
    /// about what happens to an established session has to let it establish first.
    pub(crate) failing_paths: Mutex<HashMap<String, u16>>,
    /// Armed by [`MockPc::refuse_credential`]: the password no longer opens a session.
    pub(crate) refuse_credential: std::sync::atomic::AtomicBool,
    pub(crate) rate_limit_once: Mutex<Vec<String>>,
    pub(crate) reject_select: Vec<String>,
    pub(crate) requests: Mutex<Vec<RecordedRequest>>,
    pub(crate) served_versions: HashMap<String, Vec<String>>,
    /// The status/progress sequence a mock-created task walks, one step per GET by extId.
    pub(crate) task_steps: Vec<(String, u8)>,
    pub(crate) fail_task: Vec<String>,
    pub(crate) stall_task: Vec<String>,
    /// `(kind id, action)` pairs answered 403.
    pub(crate) forbid_action: Vec<(String, String)>,
    /// One-shot `(list_path, ext_id)`: the next GET answers normally, then the entity's ETag
    /// changes, so the conditional mutation that follows gets a 412.
    pub(crate) mutate_between: Mutex<Vec<(String, String)>>,
    /// One-shots armed by [`MockPc::mutate_after_list`]: an entity that changes between two of
    /// a caller's list requests.
    pub(crate) mutate_after_list: Mutex<Vec<PendingTouch>>,
    pub(crate) tasks: Mutex<HashMap<String, MockTask>>,
    /// API paths whose answers wait for a [`Gate`], keyed the way `missing` is: without `/api`,
    /// before version mapping.
    pub(crate) gates: HashMap<String, Gate>,
    /// Whether a list answers `$select` by narrowing its rows to the fields asked for, the way
    /// a real Prism Central does. Off by default, for the reason `sorts` is: a mock that
    /// narrowed everything would change the shape of every row in every test, and the tests
    /// that are about a narrowed row ask for it by name. Without it nothing in this tree can
    /// tell a list row from a whole document, which is exactly the confusion it exists to show.
    pub(crate) narrows: bool,
    /// Whether a list answers `$orderby`. Off by default: a Prism Central that ignores an
    /// order it was asked for is a real one - several of the older API versions declare an
    /// empty `$orderby` allowlist - and it is what the scheduler's calibration exists to
    /// catch, so the default mock is the unsorted case and a test that wants the sorted one
    /// asks for it by name.
    pub(crate) sorts: bool,
    /// `(path suffix, milliseconds)`: responses to paths ending with the suffix are held back,
    /// so a test can assert what the first frame shows *before* the network answers. A path
    /// suffix rather than everything, because `connect`'s domain-manager read is a list too
    /// and delaying it would only delay the test.
    pub(crate) delay: Mutex<(String, u64)>,
    /// The per-second tier every answer advertises as `x-api-ratelimit-limit` over
    /// `x-api-ratelimit-refresh-period-seconds`. `None` is a Prism Central that says nothing
    /// about its limits, which the client has to survive without stalling.
    pub(crate) rate_limit: Option<nutsh_catalog::RateLimit>,
    /// `(x-ratelimit-limit, x-ratelimit-reset)`: the longer budget, whose `remaining` counts
    /// down over the mock's lifetime. Off by default - a countdown running out under a test
    /// about something else would look like a flake.
    pub(crate) rate_budget: Option<(u64, u64)>,
    /// The session tokens this mock is currently honouring, and how many cookie-borne requests
    /// each has answered.
    ///
    /// A valid Basic request is answered with `Set-Cookie` for a fresh token, the way the recording's
    /// Prism Central answers one: three cookies, about fifteen minutes. A request carrying a
    /// live token is authorized without a password - which is the whole point, and the only
    /// way a test can count what the client puts on the wire.
    pub(crate) sessions: Mutex<HashMap<String, usize>>,
    /// How many answers a session gives before it is dead, or `None` for a session that never
    /// expires. `Some(0)` is a Prism Central whose cookie is never accepted at all.
    pub(crate) expire_after: Option<usize>,
    /// Whether a valid Basic request is answered with `Set-Cookie` at all. A pc.7.6 Prism Central
    /// does; no spec file mentions the headers, so no version can be assumed to.
    pub(crate) issues_sessions: bool,
    /// `(api path, total)`: what this path claims in `totalAvailableResults`, whatever it
    /// actually holds. A Prism Central whose IAM list miscounts is what turned a paged walk
    /// into an endless one, and nothing else can produce a total the rows disagree with.
    pub(crate) miscounts: HashMap<String, u64>,
    /// `(api path, origin)`: this exact path answers `302` with a `Location` under `origin`,
    /// from the moment [`MockPc::redirect_from_now`] arms it. A `Mutex` for the reason
    /// `failing_paths` is one - a test about what a redirect does to an established session
    /// has to let it establish first.
    pub(crate) redirects: Mutex<HashMap<String, String>>,
}

/// An entity change armed to land partway through a caller's walk: after `answers` more list
/// answers on `path`, `field` on the entity `ext_id` becomes `value`.
///
/// A countdown rather than a wall clock, because the point of it is to be exact: a test that
/// waited for the walk's last page and then mutated would be racing the request that follows it.
pub(crate) struct PendingTouch {
    pub(crate) path: String,
    pub(crate) ext_id: String,
    pub(crate) field: String,
    pub(crate) value: String,
    pub(crate) answers: usize,
}

/// The bookkeeping behind a task the mock created. Kept beside the store rather than inside the
/// task entity, so a task's JSON is exactly what a Prism Central would return.
pub(crate) struct MockTask {
    /// Index into `task_steps` of the state the next GET by extId shows; past the end once the
    /// task is terminal.
    pub(crate) step: usize,
    pub(crate) operation: String,
    /// `None` when there is nothing to apply an effect to: a list-level create, or a fixture
    /// task registered only so it can be cancelled.
    pub(crate) target: Option<Target>,
    pub(crate) canceling: bool,
}

/// The entity a mock-created task acts on, addressed the way the store is.
pub(crate) struct Target {
    pub(crate) list_path: String,
    pub(crate) id_key: &'static str,
    pub(crate) ext_id: String,
}

/// The catalog's Task list path at the version the prism fixtures carry; the catalog's own
/// version when the tree has no prism fixtures.
fn tasks_path_of(store: &Store) -> String {
    let catalog = nutsh_catalog::kind("prism.config.Task")
        .expect("the catalog has Tasks")
        .list_path;
    match store.version_of("prism") {
        Some(v) => nutsh_catalog::with_version(catalog, &v),
        None => catalog.to_string(),
    }
}

/// What the default mock advertises as its per-second tier: the client's own absolute ceiling,
/// so the default mock is a Prism Central that never paces anybody. A suite of nine hundred
/// tests must not pay a second of real sleep for a rule none of them is about; a test that *is*
/// about pacing names a tighter tier with [`MockPcBuilder::rate_limit`], and the recording's own
/// Prism Central advertises `3 per 1s`.
///
/// Not imported from `nutsh-prism`: that crate takes this one as a dev-dependency, and the
/// dependency may not run the other way. `prism`'s own test asserts the two agree.
pub const GENEROUS: nutsh_catalog::RateLimit = nutsh_catalog::RateLimit {
    count: 30,
    per_secs: 1,
};

/// `QUEUED 0 → RUNNING 0 → RUNNING 50 → SUCCEEDED 100`.
pub(crate) fn default_steps() -> Vec<(String, u8)> {
    vec![
        ("QUEUED".into(), 0),
        ("RUNNING".into(), 0),
        ("RUNNING".into(), 50),
        ("SUCCEEDED".into(), 100),
    ]
}

pub struct MockPcBuilder {
    fixtures: PathBuf,
    username: String,
    password: String,
    unavailable: Vec<String>,
    forbidden: Vec<String>,
    failing: HashMap<String, u16>,
    missing: Vec<String>,
    failing_paths: HashMap<String, u16>,
    rate_limit_once: Vec<String>,
    reject_select: Vec<String>,
    served_versions: HashMap<String, Vec<String>>,
    miscounts: HashMap<String, u64>,
    task_steps: Vec<(String, u8)>,
    fail_task: Vec<String>,
    stall_task: Vec<String>,
    forbid_action: Vec<(String, String)>,
    mutate_between: Vec<(String, String)>,
    gates: HashMap<String, Gate>,
    repeat: Vec<(String, usize)>,
    narrows: bool,
    sorts: bool,
    rate_limit: Option<nutsh_catalog::RateLimit>,
    rate_budget: Option<(u64, u64)>,
    expire_after: Option<usize>,
    issues_sessions: bool,
}

pub struct MockPc {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl MockPc {
    /// Defaults: bundled fixtures, credentials `admin` / `secret`.
    pub fn builder() -> MockPcBuilder {
        MockPcBuilder {
            fixtures: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures"),
            username: "admin".into(),
            password: "secret".into(),
            unavailable: Vec::new(),
            forbidden: Vec::new(),
            failing: HashMap::new(),
            missing: Vec::new(),
            failing_paths: HashMap::new(),
            rate_limit_once: Vec::new(),
            reject_select: Vec::new(),
            served_versions: HashMap::new(),
            miscounts: HashMap::new(),
            task_steps: default_steps(),
            fail_task: Vec::new(),
            stall_task: Vec::new(),
            forbid_action: Vec::new(),
            mutate_between: Vec::new(),
            gates: HashMap::new(),
            repeat: Vec::new(),
            narrows: false,
            sorts: false,
            rate_limit: Some(GENEROUS),
            rate_budget: None,
            expire_after: None,
            issues_sessions: true,
        }
    }

    pub fn host(&self) -> String {
        self.addr.ip().to_string()
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.requests.lock().expect("requests lock").clone()
    }

    /// Requests whose path ends with `suffix`.
    pub fn requests_to(&self, suffix: &str) -> Vec<RecordedRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.ends_with(suffix))
            .collect()
    }

    /// One-shot: `answers` more list answers on `list_path` from now, `field` on the entity
    /// `ext_id` becomes `value` - an entity that changes partway through a caller's walk, which
    /// is the race any paging client has to survive.
    ///
    /// Armed on the running mock rather than on the builder, so a test places it at the request
    /// it means: negotiation and every earlier cycle have been answered by then, and the count
    /// starts where the test is standing.
    pub fn mutate_after_list(
        &self,
        list_path: &str,
        ext_id: &str,
        field: &str,
        value: &str,
        answers: usize,
    ) {
        self.shared
            .mutate_after_list
            .lock()
            .expect("touch lock")
            .push(PendingTouch {
                path: list_path.to_string(),
                ext_id: ext_id.to_string(),
                field: field.to_string(),
                value: value.to_string(),
                answers,
            });
    }

    /// Kill every live session now: the next request carrying one is answered 401. What a test
    /// about a session that ended partway through needs, where
    /// [`MockPcBuilder::expire_session_after`] counts answers instead.
    pub fn expire_session(&self) {
        self.shared.sessions.lock().expect("sessions lock").clear();
    }

    /// From now on, the credential is refused, whatever path presents it: what a changed
    /// password or a locked account looks like. A session already open rides until it ends.
    pub fn refuse_credential(&self) {
        self.shared
            .refuse_credential
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// From now on, this exact API path answers `status`. The builder's
    /// [`MockPcBuilder::fail_path`] is the same thing before the first request; this is the one
    /// a test reaches for when the session has to be working before it breaks.
    pub fn fail_from_now(&self, api_path: &str, status: u16) {
        self.shared
            .failing_paths
            .lock()
            .expect("failing paths lock")
            .insert(api_path.to_string(), status);
    }

    /// From now on, this exact API path answers `302` with a `Location` pointing at the same
    /// path under `origin` - another host entirely. What a Prism Central behind a gateway that
    /// has been told to send everything somewhere else looks like, and the only way to see
    /// what a client does with the session cookie it is holding when one arrives.
    pub fn redirect_from_now(&self, api_path: &str, origin: &str) {
        self.shared
            .redirects
            .lock()
            .expect("redirects lock")
            .insert(api_path.to_string(), origin.to_string());
    }

    /// Hold responses to paths ending with `path_suffix` back by `delay`, from now on.
    ///
    /// Set on the running mock rather than on the builder, so a test that wants a second run
    /// against the same `host:port` - which is half a cache's identity - reaches this one
    /// instead of hoping a freed port can be re-bound.
    pub fn set_delay(&self, path_suffix: &str, delay: std::time::Duration) {
        *self.shared.delay.lock().expect("delay lock") = (
            path_suffix.to_string(),
            u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
        );
    }
}

impl Drop for MockPc {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockPcBuilder {
    pub fn fixtures(mut self, dir: impl Into<PathBuf>) -> Self {
        self.fixtures = dir.into();
        self
    }

    pub fn credentials(mut self, username: &str, password: &str) -> Self {
        self.username = username.into();
        self.password = password.into();
        self
    }

    /// Every path under `/api/<namespace>/` answers 404.
    pub fn unavailable_namespace(mut self, namespace: &str) -> Self {
        self.unavailable.push(namespace.into());
        self
    }

    /// Every path under `/api/<namespace>/` answers 403: the namespace is served, this
    /// account may not read it.
    pub fn forbid_namespace(mut self, namespace: &str) -> Self {
        self.forbidden.push(namespace.into());
        self
    }

    /// Answer `$select` by narrowing the rows to the fields it names, the way a real Prism
    /// Central does. A list row is then a projection and not a whole document, which is the
    /// difference every caller that reads a field off one has to survive.
    pub fn narrows(mut self) -> Self {
        self.narrows = true;
        self
    }

    /// This exact API path reports `total` in `totalAvailableResults` however many rows it
    /// actually serves: a Prism Central that miscounts its own list, which is the only thing a
    /// client walking that total has to survive.
    pub fn miscount(mut self, api_path: &str, total: u64) -> Self {
        self.miscounts.insert(api_path.into(), total);
        self
    }

    /// For `namespace`, serve exactly these API versions, the way an older Prism Central
    /// does: a request for a listed version is answered from the fixture tree whatever
    /// version its files carry, and a request for any other version is a 404. Namespaces
    /// without a setting keep the exact-path behaviour.
    pub fn serve_versions(mut self, namespace: &str, versions: &[&str]) -> Self {
        self.served_versions.insert(
            namespace.into(),
            versions.iter().map(|v| (*v).to_string()).collect(),
        );
        self
    }

    /// Every path under `/api/<namespace>/` answers `status`: a namespace whose service is
    /// down (503) or overloaded (429), as opposed to one that is not there at all.
    pub fn fail_namespace(mut self, namespace: &str, status: u16) -> Self {
        self.failing.insert(namespace.into(), status);
        self
    }

    /// This exact API path (without `/api`, as the client sends it, before version mapping)
    /// answers 404: a served namespace that lacks one kind, the way the recording's multidomain
    /// serves `management/local-domain` and 404s `config/locations`.
    pub fn missing_path(mut self, api_path: &str) -> Self {
        self.missing.push(api_path.into());
        self
    }

    /// This exact API path (without `/api`, as `missing_path` takes it) answers `status`: one
    /// endpoint of a served namespace that is down or refusing, as opposed to one that is not
    /// there at all. `missing_path` is this with 404.
    pub fn fail_path(mut self, api_path: &str, status: u16) -> Self {
        self.failing_paths.insert(api_path.into(), status);
        self
    }

    /// This API path (without `/api`) answers 400 to any request carrying a `$select`, and
    /// serves normally to one that does not.
    ///
    /// What a Prism Central does with a narrowing its gateway will not accept, which the specs
    /// do not promise never happens: `list_params.select` is what the spec file declares, and a
    /// deployment is free to disagree with it.
    pub fn reject_select(mut self, api_path: &str) -> Self {
        self.reject_select.push(api_path.into());
        self
    }

    /// The first request to this API path (without `/api`) answers 429 with `Retry-After: 0`.
    pub fn rate_limit_once(mut self, api_path: &str) -> Self {
        self.rate_limit_once.push(api_path.into());
        self
    }

    /// Hold every answer for exactly `api_path` behind `gate`, which the test opens by hand. The
    /// path is the one [`MockPcBuilder::missing_path`] takes: without `/api`, as the client sends
    /// it, before version mapping.
    ///
    /// The match is exact, so an entity GET under a held list path is a different path and is not
    /// held. The request is recorded before the gate, so [`MockPc::requests_to`] counts one that
    /// is still waiting, and it is held *before* it is authenticated, so a test can park a
    /// handful of requests that are all holding one session and then end that session with
    /// [`MockPc::expire_session`] before it lets any of them go.
    pub fn hold_path(mut self, api_path: &str, gate: &Gate) -> Self {
        self.gates.insert(api_path.to_string(), gate.clone());
        self
    }

    /// Repeat the fixture's entities, each with a fresh extId, until the list at `api_path` holds
    /// `n` of them: a five-thousand-row audit list without a five-thousand-row file in the
    /// repository. The path is the fixture's own, version and all, as `Store::load` keys it.
    ///
    /// A path with no fixture is left alone rather than filled with invented rows, which is what
    /// a mistyped path deserves, and `n == 0` is the same no-op rather than a way to empty a list.
    pub fn repeat_fixture(mut self, api_path: &str, n: usize) -> Self {
        self.repeat.push((api_path.to_string(), n));
        self
    }

    /// Advertise `count` requests per `per_secs` on every answer, as `x-api-ratelimit-limit`
    /// and `x-api-ratelimit-refresh-period-seconds`. A recorded Prism Central says `3 per 1s`;
    /// [`GENEROUS`] is the default.
    ///
    /// Advertised, not enforced: the mock answers whatever it is asked, so a test that exceeds
    /// the tier sees the requests it made rather than a 429 that would hide the overrun.
    pub fn rate_limit(mut self, count: u32, per_secs: u32) -> Self {
        self.rate_limit = Some(nutsh_catalog::RateLimit { count, per_secs });
        self
    }

    /// Advertise nothing at all: a Prism Central whose answers carry no rate-limit headers,
    /// which is every version the specs describe - the headers are in none of the 93 spec files.
    pub fn no_rate_limit_headers(mut self) -> Self {
        self.rate_limit = None;
        self
    }

    /// Never set a session cookie: a Prism Central that offers nothing to reuse, which every
    /// version has to be assumed to be until one is seen to do otherwise. A client that reuses
    /// sessions must degrade here to carrying the credential on every request, and must not
    /// spend an extra round trip per request discovering that afresh.
    pub fn no_session_cookies(mut self) -> Self {
        self.issues_sessions = false;
        self
    }

    /// A session answers `n` requests and is then dead, so the `n + 1`th request carrying it is
    /// answered 401 - which is expiry, not a refused credential, and the client has to tell
    /// those two apart. `0` is a Prism Central whose cookie is never accepted at all.
    ///
    /// The default is a session that never expires. A real one lasts about fifteen minutes.
    pub fn expire_session_after(mut self, n: usize) -> Self {
        self.expire_after = Some(n);
        self
    }

    /// Advertise the longer budget too: `x-ratelimit-limit: limit`, `x-ratelimit-reset: reset`,
    /// and an `x-ratelimit-remaining` that counts down one per request served and stays at zero
    /// once it is spent. Off by default.
    pub fn rate_budget(mut self, limit: u64, reset_secs: u64) -> Self {
        self.rate_budget = Some((limit, reset_secs));
        self
    }

    /// Honour `$orderby` on lists: a single top-level property, `asc` or `desc`, sorted the
    /// way a Prism Central that implements the parameter would. Off by default; see
    /// `Shared::sorts`.
    pub fn honours_orderby(mut self) -> Self {
        self.sorts = true;
        self
    }

    /// Tasks for `operation` end `FAILED` with `errorMessages` and apply no effect.
    pub fn fail_task(mut self, operation: &str) -> Self {
        self.fail_task.push(operation.into());
        self
    }

    /// Tasks for `operation` stay `QUEUED 0` forever, unless cancelled.
    pub fn stall_task(mut self, operation: &str) -> Self {
        self.stall_task.push(operation.into());
        self
    }

    /// Replace the status/progress sequence every mock-created task walks.
    pub fn task_steps(mut self, steps: &[(&str, u8)]) -> Self {
        self.task_steps = steps.iter().map(|(s, p)| ((*s).to_string(), *p)).collect();
        self
    }

    /// One-shot: the next GET of this entity answers normally, then its ETag changes, so the
    /// conditional mutation that follows gets a 412.
    pub fn mutate_between_get_and_post(mut self, list_path: &str, ext_id: &str) -> Self {
        self.mutate_between.push((list_path.into(), ext_id.into()));
        self
    }

    /// This kind's action answers 403 with the standard envelope.
    pub fn forbid_action(mut self, kind_id: &str, action: &str) -> Self {
        self.forbid_action.push((kind_id.into(), action.into()));
        self
    }

    pub async fn start(self) -> MockPc {
        let mut store = Store::load(&self.fixtures).expect("loading fixtures");
        for (path, n) in &self.repeat {
            let items = store.list(path).unwrap_or_default().to_vec();
            if items.is_empty() || *n == 0 {
                continue;
            }
            let id_key = handler::id_key_for(path);
            let grown: Vec<Value> = (0..*n)
                .map(|i| {
                    let mut e = items[i % items.len()].clone();
                    // Deterministic, so a snapshot over a repeated fixture is stable.
                    e.as_object_mut()
                        .unwrap_or_else(|| panic!("repeat_fixture: {path} holds a non-object"))
                        .insert(
                            id_key.to_string(),
                            Value::String(format!("00000000-0000-4000-8000-{i:012x}")),
                        );
                    e
                })
                .collect();
            store.insert(path, grown);
        }
        let tasks_path = tasks_path_of(&store);
        let shared = Arc::new(Shared {
            store: RwLock::new(store),
            tasks_path,
            username: self.username,
            password: self.password,
            unavailable: self.unavailable,
            forbidden: self.forbidden,
            failing: self.failing,
            missing: self.missing,
            failing_paths: Mutex::new(self.failing_paths),
            refuse_credential: std::sync::atomic::AtomicBool::new(false),
            rate_limit_once: Mutex::new(self.rate_limit_once),
            reject_select: self.reject_select,
            requests: Mutex::new(Vec::new()),
            served_versions: self.served_versions,
            task_steps: self.task_steps,
            fail_task: self.fail_task,
            stall_task: self.stall_task,
            forbid_action: self.forbid_action,
            mutate_between: Mutex::new(self.mutate_between),
            mutate_after_list: Mutex::new(Vec::new()),
            tasks: Mutex::new(HashMap::new()),
            gates: self.gates,
            sorts: self.sorts,
            delay: Mutex::new((String::new(), 0)),
            rate_limit: self.rate_limit,
            rate_budget: self.rate_budget,
            miscounts: self.miscounts,
            narrows: self.narrows,
            redirects: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            expire_after: self.expire_after,
            issues_sessions: self.issues_sessions,
        });
        let app = axum::Router::new()
            .fallback(handler::handle)
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        MockPc { addr, shared, task }
    }
}
