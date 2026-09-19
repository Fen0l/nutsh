//! The credential and the session: what a request carries, one presentation at a time, and
//! what a 401 means depending on what it carried.

use super::*;

/// How many renewals one request may wait through before its 401 goes to the caller: a
/// session can end again while it is queued behind the last renewal. Bounded, or it is a loop.
pub(super) const RENEWALS: usize = 2;

/// How many answered URLs the jar remembers for the liveness check.
pub(super) const PROBE_URLS: usize = 4;

/// The budget key a liveness check spends from.
pub(super) const SESSION_PROBE_TEMPLATE: &str = "/session-probe";

/// Sessions that came to nothing before reuse is given up: a cookie refused before it ever
/// authenticated, or a presentation that opened none. After this every request carries the
/// credential with no gate, as before session reuse existed. Read lazily off two monotonic
/// facts, so a refusal that overtakes its own burst costs one presentation, not the feature.
pub(super) const GIVE_UP_RENEWALS: u32 = 2;

/// The session cookies a Prism Central hands out, replayed instead of the password. pc.7.6
/// sets three (`NTNX_MERCURY_IAM_SESSION`, `NTNX_MERCURY_IAM_REFRESH_TOKEN`, `NTNX_IAM_SESSION`),
/// none in any spec, so a Prism Central that sets none leaves every request carrying the
/// credential. Not reqwest's `Jar`: this one is clearable, and `generation` makes `N` stale
/// holders renew once. Attributes are ignored: one client, one Prism Central, all under `/api`.
#[derive(Debug)]
pub(super) struct SessionJar {
    /// The one origin this jar hands a cookie to or takes one from. Read by the cookie layer
    /// twice and by [`may_carry`] a third time, against the built request - the session travels
    /// as a header `attempt` sets itself, so that check is the only one that sees the URL.
    pub(super) origin: Origin,
    pub(super) inner: std::sync::RwLock<Jarred>,
}

/// A session rides only to the origin its jar came from. Unreachable today - every URL is
/// built from the profile's base - and written for the day a `Location` or an `_links` href
/// is followed by hand.
pub(super) fn may_carry(origin: &Origin, url: &Url, presented: bool) -> bool {
    presented || origin.is(url)
}

/// Scheme, host and port: the three things that have to match before a session cookie may
/// cross between a request and this jar.
#[derive(Debug)]
pub(super) struct Origin {
    pub(super) scheme: &'static str,
    pub(super) host: String,
    pub(super) port: u16,
}

impl Origin {
    pub(super) fn of(profile: &Profile) -> Origin {
        Origin {
            scheme: if profile.plain_http { "http" } else { "https" },
            host: profile.host.clone(),
            port: profile.port,
        }
    }

    /// Whether `url` is this Prism Central and not some other host. `port_or_known_default`
    /// rather than `port`, because a `Url` drops `:443` from an https address and would
    /// otherwise never match a profile that spells it out.
    pub(super) fn is(&self, url: &Url) -> bool {
        url.scheme() == self.scheme
            && url.port_or_known_default() == Some(self.port)
            && url
                .host_str()
                .is_some_and(|h| h.eq_ignore_ascii_case(&self.host))
    }
}

#[derive(Debug, Default)]
pub(super) struct Jarred {
    /// `name` to `value`, in name order so the header is stable.
    pub(super) cookies: BTreeMap<String, String>,
    /// Bumped by [`SessionJar::expire`] alone. A request captures it before it goes out and
    /// hands it back with its 401, so only the first of `N` requests holding one stale session
    /// is the one that renews it.
    pub(super) generation: u64,
    /// Whether a request riding a session from this Prism Central has ever been answered. Once
    /// true it stays true: one answer settles that cookies authenticate this API.
    pub(super) ever_answered: bool,
    /// The last few URLs a session-riding request was answered on, oldest first: where a 401
    /// is checked against before it is taken for an expiry. See [`Client::session_alive`].
    pub(super) ok_urls: std::collections::VecDeque<String>,
    /// How many sessions have been refused. Only ever grows.
    pub(super) renewals: u32,
}

impl SessionJar {
    pub(super) fn read(&self) -> std::sync::RwLockReadGuard<'_, Jarred> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn write(&self) -> std::sync::RwLockWriteGuard<'_, Jarred> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether session reuse is still worth attempting: see [`GIVE_UP_RENEWALS`]. Safe to read
    /// outside the gate, both facts only move one way; `false` is the degraded mode.
    pub(super) fn usable(&self) -> bool {
        let jar = self.read();
        jar.ever_answered || jar.renewals < GIVE_UP_RENEWALS
    }

    /// The exact `Cookie` header and its generation, read under one lock, or `None` when there is
    /// nothing to ride. The header travels with the request: a jar emptied between decision and
    /// send would put out a request carrying nothing, whose 401 buys a second renewal.
    pub(super) fn ride(&self) -> Option<(u64, header::HeaderValue)> {
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
    pub(super) fn answered(&self, url: &str) {
        self.write().ever_answered = true;
        self.remember_ok(url);
    }

    /// A URL this account was answered on, whatever the request carried: a presentation that
    /// was answered opened the session the next check rides, so its URL is as good a place to
    /// check as any.
    pub(super) fn remember_ok(&self, url: &str) {
        let mut jar = self.write();
        if jar.ok_urls.back().is_some_and(|u| u == url) {
            return;
        }
        jar.ok_urls.retain(|u| u != url);
        jar.ok_urls.push_back(url.to_string());
        if jar.ok_urls.len() > PROBE_URLS {
            jar.ok_urls.pop_front();
        }
    }

    /// A URL this session has answered on, other than `not`: what a liveness check asks.
    /// `None` when the session has proven nothing away from `not`, in which case a 401 there
    /// is taken for an expiry, as it always was.
    pub(super) fn probe_url(&self, not: &str) -> Option<String> {
        self.read()
            .ok_urls
            .iter()
            .rev()
            .find(|u| u.as_str() != not)
            .cloned()
    }

    /// A presentation was answered. One that left the jar empty opened no session, which counts
    /// like a refused one for [`GIVE_UP_RENEWALS`]: otherwise a Prism Central that sets no cookie
    /// would take the gate for every request, for the life of the session.
    pub(super) fn presented(&self) {
        let mut jar = self.write();
        if jar.cookies.is_empty() {
            jar.renewals = jar.renewals.saturating_add(1);
        }
    }

    /// The session behind `generation` was refused: drop it so the next request through the gate
    /// renews. A caller whose generation moved on drops nothing; the refusal still counts.
    pub(super) fn expire(&self, generation: u64) {
        let mut jar = self.write();
        jar.renewals = jar.renewals.saturating_add(1);
        if jar.generation != generation {
            return;
        }
        jar.cookies.clear();
        jar.generation += 1;
    }
}

/// What this Prism Central has said about the credential. Every transition is one way, and
/// the session is the jar's business, so nothing here unwinds when a session ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthState {
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

/// The single-flight authentication: one permit, and only its holder may present the
/// credential. Concurrent probes each carrying the password would be one authentication
/// event per request against a lockout policy. See [`Client::carry`].
pub(super) struct Auth {
    /// A `Mutex` held for nanoseconds and never across an await, on the precedent of
    /// `Client::buckets`.
    pub(super) state: std::sync::Mutex<AuthState>,
    /// One permit, taken by every request that has no session to ride.
    pub(super) gate: tokio::sync::Semaphore,
}

impl Auth {
    pub(super) fn new() -> Auth {
        Auth {
            state: std::sync::Mutex::new(AuthState::Unauthenticated),
            gate: tokio::sync::Semaphore::new(1),
        }
    }

    /// A poisoned state is taken as it stands, for the reason `Client::buckets` gives. The
    /// conservative reading is the one already there: a panic cannot invent an `Established`.
    pub(super) fn state(&self) -> AuthState {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What one answer proved about the credential. `presented` says whether this request
    /// actually carried one: a 401 on a request that did is the credential being refused, and
    /// there is no state in which that becomes untrue again.
    pub(super) fn settle(&self, presented: bool, status: StatusCode) {
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
pub(super) enum Pass {
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
pub(super) enum Carry {
    /// The credential. Only ever built while this request holds the auth permit, or once
    /// session reuse has been given up for good - see [`Client::carry`].
    Credential,
    /// This exact session cookie, and the generation it belongs to.
    Session(u64, header::HeaderValue),
}

/// What one trip through [`Client::attempt`] settled.
pub(super) enum Attempt {
    Answered((StatusCode, HeaderMap, Vec<u8>)),
    /// A 401 on a request that carried no credential: the session it was riding has ended, and
    /// this request has not been answered yet.
    SessionEnded,
}

/// What a request carried, which is the field that answers "which call was it that failed the
/// credentials". A 401 means two different things depending on this word, and the asymmetry
/// `Client::send` rests on is exactly that difference: `Presented` and 401 is a credential the
/// far end refused, terminal and never retried; `Session` and 401 is a session that had ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Carried {
    /// It presented the credential.
    Presented,
    /// It rode a session cookie the credential had already opened.
    Session,
    /// It never went out: this client had already been refused, and the one thing that must
    /// never happen next is asking again.
    Refused,
}

impl Carried {
    pub(super) fn word(self) -> &'static str {
        match self {
            Carried::Presented => "presented",
            Carried::Session => "session",
            Carried::Refused => "refused",
        }
    }
}

/// One event per request, from the one funnel every request goes through. Method, URL,
/// status, round trip and what it carried - never a header, a cookie or a body; `Carried`
/// says which credential went out without saying what it was (`tests/log.rs` proves it).
/// A 401 or a 5xx is a `warn`, so finding one does not mean reading at `debug`.
pub(super) fn trace_request(
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
pub(super) fn refused_before_the_network(template: &'static str) {
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
pub(super) fn path_of(url: &str) -> String {
    url.find("://")
        .and_then(|i| url[i + 3..].find('/').map(|j| url[i + 3 + j..].to_string()))
        .unwrap_or_else(|| url.to_string())
}

/// A ring entry for a request this client refused to send.
pub(super) fn refused_call(template: &'static str) -> crate::metrics::Call {
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

pub(super) fn safe_url(url: &reqwest::Url) -> String {
    if url.username().is_empty() && url.password().is_none() {
        return url.to_string();
    }
    let mut clean = url.clone();
    let _ = clean.set_password(None);
    let _ = clean.set_username("");
    clean.to_string()
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

impl Client {
    /// Whether the session behind `cookie` still answers: the same cookie, no credential, on
    /// a URL it has already been answered on other than `not`. `false` when there is no such
    /// URL - nothing is proven, so the caller treats the 401 as the expiry it may be. A pipe
    /// failure is `true`: without an answer no password goes out, and a session that really
    /// ended is found out by the next request.
    pub(super) async fn session_alive(&self, cookie: &header::HeaderValue, not: &str) -> bool {
        let Some(url) = self.jar.probe_url(not) else {
            return false;
        };
        self.acquire(&HttpMethod::GET, SESSION_PROBE_TEMPLATE, RateLimit::DEFAULT)
            .await;
        let began = std::time::Instant::now();
        self.metrics.record_started(began);
        let req = self
            .http
            .get(&url)
            .header(header::COOKIE, cookie.clone())
            .header(header::ACCEPT, "application/json");
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                self.metrics
                    .record_completed(std::time::Instant::now().saturating_duration_since(began));
                self.trace(
                    &HttpMethod::GET,
                    &url,
                    Carried::Session,
                    began,
                    false,
                    Ok(status),
                );
                status != StatusCode::UNAUTHORIZED
            }
            Err(e) => {
                self.metrics.record_failed(std::time::Instant::now());
                let e = self.map_transport(e);
                self.trace(
                    &HttpMethod::GET,
                    &url,
                    Carried::Session,
                    began,
                    false,
                    Err(&e),
                );
                true
            }
        }
    }

    /// The log line and the ring entry for one request, from the same facts.
    pub(super) fn trace(
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

    /// Whether this Prism Central has refused the credential. Once true it stays true, every
    /// `Client::send` fails without a request, and the pollers that are not subscriptions read
    /// it to end their loops.
    pub fn auth_rejected(&self) -> bool {
        self.auth.state() == AuthState::Rejected || !self.valve.open()
    }

    /// One request through the gate and the session, with one renewal if the session it rode had
    /// ended. A 401 on a request that carried the credential is terminal; a 401 on a session is
    /// that session ending, and `N` holders of the same stale cookie renew once between them.
    pub(super) async fn send(
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

    /// What this request carries, and the permit it holds while it does. A credential goes out
    /// only while this request holds the permit, and the holder presents exactly when the jar has
    /// nothing to ride: both read behind the permit, so one emptied jar buys one presentation
    /// however many requests are in flight. The jar, not [`AuthState`], decides. The exception is
    /// the degraded mode after [`GIVE_UP_RENEWALS`]: every request carries the credential, no gate.
    pub(super) async fn carry(
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

    pub(super) async fn attempt(
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
        // The session travels with the request rather than being looked up at dispatch: the jar
        // can be emptied by another task in between, and a request carrying nothing is a 401
        // that buys a renewal. `riding` and the credential are one fact read from both ends.
        let (riding, cookie, req) = match carry {
            Carry::Credential => (
                None,
                None,
                req.basic_auth(&self.username, Some(&self.password)),
            ),
            Carry::Session(generation, cookie) => (
                Some(generation),
                Some(cookie.clone()),
                req.header(header::COOKIE, cookie),
            ),
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
            // Proven before a password goes out: the same session on a URL it already answered.
            // If that still answers, this 401 is the endpoint's verdict on the account, not an
            // expiry - a renewal would present the credential to an endpoint that answers 401
            // whatever it carries, and read that as a refused password.
            if let Some(cookie) = cookie.as_ref()
                && self.session_alive(cookie, &url).await
            {
                let status = resp.status();
                let body = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
                self.metrics
                    .record_completed(std::time::Instant::now().saturating_duration_since(began));
                self.trace(&method, &url, carried, began, paced, Ok(status));
                return Err(PrismError::Denied(
                    envelope::error_message(&body).unwrap_or_else(|| path_of(&url)),
                ));
            }
            self.metrics.record_failed(std::time::Instant::now());
            self.trace(&method, &url, carried, began, paced, Ok(resp.status()));
            self.jar.expire(generation);
            return Ok(Attempt::SessionEnded);
        }
        if !presented {
            self.jar.answered(&url);
        } else if resp.status() != StatusCode::UNAUTHORIZED {
            self.jar.remember_ok(&url);
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
}
