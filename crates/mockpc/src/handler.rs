//! One fallback handler that emulates the v4 conventions the client relies on.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::store::{Store, etag_of};
use crate::{MockTask, RecordedRequest, Shared, Target};

/// When a mock-created task is created. Step `n` lands `n` seconds later and the terminal state
/// at `DONE_AT`, so a Tasks table sorted by last-updated follows the progress.
const CREATED_AT: &str = "2026-09-05T09:59:00Z";
/// When a mock-created task reaches a terminal state.
const DONE_AT: &str = "2026-09-05T10:00:00Z";

fn step_time(step: usize) -> String {
    format!("2026-09-05T09:59:{:02}Z", step.min(59))
}

/// Every answer carries the rate-limit headers the mock advertises, exactly as a Prism Central
/// puts them on every response - a 200, a 404 and a 429 alike. They are the outer wrapper and
/// not a branch of the routing below, so no early return can forget them: a client that learns
/// its pacing from the headers must learn it from *every* answer, including the ones it got
/// wrong.
pub async fn handle(
    State(state): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut response = route(State(state.clone()), method, uri, headers, body).await;
    advertise(&state, response.headers_mut());
    response
}

/// Write the advertised budgets onto one answer. Nothing is written when the mock advertises
/// no per-second tier, which is how a test spells "a Prism Central that says nothing about its
/// limits".
fn advertise(state: &Shared, headers: &mut HeaderMap) {
    if let Some(rate) = state.rate_limit {
        insert(headers, "x-api-ratelimit-limit", rate.count);
        insert(
            headers,
            "x-api-ratelimit-refresh-period-seconds",
            rate.per_secs,
        );
        insert(
            headers,
            "x-api-ratelimit-remaining",
            rate.count.saturating_sub(1),
        );
    }
    let Some((limit, reset)) = state.rate_budget else {
        return;
    };
    // Counted off the recorded requests, so the countdown is exactly what this mock served and
    // a test can name the answer on which the budget runs out.
    let served = u64::try_from(state.requests.lock().expect("requests lock").len()).unwrap_or(0);
    insert(headers, "x-ratelimit-limit", limit);
    insert(
        headers,
        "x-ratelimit-remaining",
        limit.saturating_sub(served),
    );
    insert(headers, "x-ratelimit-reset", reset);
}

fn insert(headers: &mut HeaderMap, name: &'static str, value: impl std::fmt::Display) {
    if let Ok(v) = header::HeaderValue::from_str(&value.to_string()) {
        headers.insert(header::HeaderName::from_static(name), v);
    }
}

async fn route(
    State(state): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let query: Vec<(String, String)> = uri
        .query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    let body_json: Option<Value> = if body.is_empty() {
        None
    } else {
        serde_json::from_slice(&body).ok()
    };
    state
        .requests
        .lock()
        .expect("requests lock")
        .push(RecordedRequest {
            method: method.to_string(),
            path: path.clone(),
            query: query.clone(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect(),
            body: body_json.clone(),
            at: std::time::Instant::now(),
        });

    // After the recording, before any routing: a held-back answer is still a request that
    // arrived, and a test that waits for one must not have to wait out the delay to see it.
    let (suffix, ms) = state.delay.lock().expect("delay lock").clone();
    if ms > 0 && !suffix.is_empty() && path.ends_with(&suffix) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }

    // Held, if a test asked for it - before the credential is so much as looked at, so a test
    // can park requests that are holding a session and then end that session underneath them.
    // No lock is held across the await: the gate is its own.
    if let Some(gate) = path.strip_prefix("/api").and_then(|p| state.gates.get(p)) {
        gate.acquire().await;
    }

    let issue = match authorize(&state, &headers) {
        // A valid password opens a session, exactly as A recorded Prism Central does: the
        // answer to a plain v4 Basic request carries three `Set-Cookie` headers.
        Authorization::Credential if state.issues_sessions => Some(new_session(&state)),
        Authorization::Credential => None,
        Authorization::Session => None,
        Authorization::Refused => {
            return error(StatusCode::UNAUTHORIZED, "Authentication required");
        }
    };
    // The cookies go on whatever the routing below answers - a 404 and a 500 included. A
    // gateway authenticates before it routes, so an answer it got wrong still carries the
    // session it opened; a mock that only cookied its successes would make every client that
    // reuses sessions re-authenticate after one failed request.
    let mut response = serve(&state, &method, &path, &query, &headers, body_json.as_ref()).await;
    if let Some(token) = issue {
        set_session_cookies(response.headers_mut(), &token);
    }
    response
}

async fn serve(
    state: &Shared,
    method: &Method,
    path: &str,
    query: &[(String, String)],
    headers: &HeaderMap,
    body_json: Option<&Value>,
) -> Response {
    let Some(api_path) = path.strip_prefix("/api") else {
        return error(StatusCode::NOT_FOUND, "not an API path");
    };
    let namespace = api_path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    if state.unavailable.contains(&namespace) {
        return error(
            StatusCode::NOT_FOUND,
            &format!("namespace {namespace} is not available on this Prism Central"),
        );
    }
    if state.forbidden.contains(&namespace) {
        return error(
            StatusCode::FORBIDDEN,
            &format!("namespace {namespace} is not permitted for this account"),
        );
    }
    if let Some(&status) = state.failing.get(&namespace) {
        let status = StatusCode::from_u16(status).expect("valid status");
        let message = format!(
            "{namespace} is failing with HTTP {status}",
            status = status.as_u16()
        );
        // A real PC says how long to wait; without the header a client's retry sleeps its
        // default, which would make every 429 test wait for real.
        if status == StatusCode::TOO_MANY_REQUESTS {
            return (
                status,
                [(header::RETRY_AFTER, "0")],
                Json(error_body(status, &message)),
            )
                .into_response();
        }
        return error(status, &message);
    }
    if state.missing.iter().any(|p| p == api_path) {
        return error(StatusCode::NOT_FOUND, &format!("no such path {api_path}"));
    }
    let failing = state
        .failing_paths
        .lock()
        .expect("failing paths lock")
        .get(api_path)
        .copied();
    if let Some(status) = failing {
        let status = StatusCode::from_u16(status).expect("valid status");
        let message = format!(
            "{api_path} is failing with HTTP {status}",
            status = status.as_u16()
        );
        // A real PC says how long to wait, and `fail_namespace` already does: without the
        // header the client's retry sleeps its five-second default, which would make every
        // path-level 429 test wait for real.
        if status == StatusCode::TOO_MANY_REQUESTS {
            return (
                status,
                [(header::RETRY_AFTER, "0")],
                Json(error_body(status, &message)),
            )
                .into_response();
        }
        return error(status, &message);
    }
    let redirect = state
        .redirects
        .lock()
        .expect("redirects lock")
        .get(api_path)
        .cloned();
    if let Some(origin) = redirect {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, format!("{origin}{path}"))],
            Json(error_body(StatusCode::FOUND, "moved")),
        )
            .into_response();
    }
    if state.reject_select.iter().any(|p| p == api_path)
        && query.iter().any(|(k, _)| k == "$select")
    {
        return error(StatusCode::BAD_REQUEST, "$select is not supported here");
    }
    {
        let mut once = state.rate_limit_once.lock().expect("rate limit lock");
        if let Some(i) = once.iter().position(|p| p == api_path) {
            once.remove(i);
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "0")],
                Json(error_body(StatusCode::TOO_MANY_REQUESTS, "rate limited")),
            )
                .into_response();
        }
    }

    let api_path = match served_path(state, &namespace, api_path) {
        Ok(p) => p,
        Err(message) => return error(StatusCode::NOT_FOUND, &message),
    };
    match *method {
        Method::GET => get(state, &api_path, query),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE => {
            mutate(state, method, &api_path, headers, body_json)
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

/// What a request proved about itself.
enum Authorization {
    /// It carried the right username and password.
    Credential,
    /// It carried a session cookie this mock is still honouring.
    Session,
    Refused,
}

/// The three cookies a pc.7.6 Prism Central sets on a valid v4 Basic request, all
/// `Expires` about fifteen minutes out.
const SESSION_COOKIES: [&str; 3] = [
    "NTNX_MERCURY_IAM_SESSION",
    "NTNX_MERCURY_IAM_REFRESH_TOKEN",
    "NTNX_IAM_SESSION",
];

/// A fresh session, registered as live. The token is a uuid rather than a counter so two
/// sessions in one test cannot collide.
fn new_session(state: &Shared) -> String {
    let token = uuid::Uuid::new_v4().to_string();
    state
        .sessions
        .lock()
        .expect("sessions lock")
        .insert(token.clone(), 0);
    token
}

/// The real thing sets `Secure; HttpOnly; SameSite=Lax`. `Secure` is left off here because the
/// mock is plain HTTP and a client that honoured it would never send the cookie back - which
/// would make this a test of the scheme rather than of the session.
fn set_session_cookies(headers: &mut HeaderMap, token: &str) {
    for name in SESSION_COOKIES {
        if let Ok(v) = header::HeaderValue::from_str(&format!(
            "{name}={token}; Path=/; HttpOnly; SameSite=Lax"
        )) {
            headers.append(header::SET_COOKIE, v);
        }
    }
}

fn authorize(state: &Shared, headers: &HeaderMap) -> Authorization {
    if authorized(state, headers) {
        return Authorization::Credential;
    }
    let Some(token) = session_token(headers) else {
        return Authorization::Refused;
    };
    let mut sessions = state.sessions.lock().expect("sessions lock");
    let Some(answered) = sessions.get_mut(&token) else {
        return Authorization::Refused;
    };
    // Expiry is counted in answers rather than measured against a clock, so a test names the
    // request on which the session dies instead of racing one.
    if state.expire_after.is_some_and(|n| *answered >= n) {
        sessions.remove(&token);
        return Authorization::Refused;
    }
    *answered += 1;
    Authorization::Session
}

/// The session token out of the `Cookie` header, by the first of [`SESSION_COOKIES`] present.
fn session_token(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        SESSION_COOKIES
            .contains(&name.trim())
            .then(|| value.trim().to_string())
    })
}

/// Applies `serve_versions`: refuses a version the namespace does not serve and maps a
/// served one onto the version the fixture files carry (or the catalog's, when the
/// namespace has no fixtures), so one fixture tree stands in for every PC release. Later
/// 404s therefore name the mapped path, not the one the client sent.
fn served_path(state: &Shared, namespace: &str, api_path: &str) -> Result<String, String> {
    let Some(served) = state.served_versions.get(namespace) else {
        return Ok(api_path.to_string());
    };
    let Some(requested) = nutsh_catalog::version_in(api_path) else {
        return Err(format!("{namespace}: no API version in the path"));
    };
    if !served.iter().any(|v| v == requested) {
        return Err(format!(
            "{namespace} {requested} is not served by this Prism Central"
        ));
    }
    let target = state
        .store
        .read()
        .expect("store lock")
        .version_of(namespace)
        .or_else(|| nutsh_catalog::namespace(namespace).map(|n| n.version.to_string()));
    Ok(match target {
        Some(t) => nutsh_catalog::with_version(api_path, &t),
        None => api_path.to_string(),
    })
}

fn authorized(state: &Shared, headers: &HeaderMap) -> bool {
    use base64::Engine;
    let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(b64) = v.strip_prefix("Basic ") else {
        return false;
    };
    if state
        .refuse_credential
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return false;
    }
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return false;
    };
    String::from_utf8(raw)
        .map(|s| s == format!("{}:{}", state.username, state.password))
        .unwrap_or(false)
}

fn get(state: &Shared, api_path: &str, query: &[(String, String)]) -> Response {
    let (page, limit) = match paging(query) {
        Ok(paging) => paging,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    {
        // The guard lives for the answer only: `advance` and `touch_after_list` below need the
        // write lock.
        let listed = {
            let store = state.store.read().expect("store lock");
            store.list(api_path).map(|items| {
                let items = if state.sorts {
                    ordered(items, query)
                } else {
                    Cow::Borrowed(items)
                };
                let items = if state.narrows {
                    narrowed(&items, api_path, query)
                } else {
                    items
                };
                list_response(&items, page, limit, state.miscounts.get(api_path).copied())
            })
        };
        if let Some(response) = listed {
            // The answer above is honest; the change lands behind it, so the request that
            // follows sees an entity the walk read at its old value.
            touch_after_list(state, api_path);
            return response;
        }
    }
    if let Some((list_path, ext_id)) = api_path.rsplit_once('/') {
        if list_path == state.tasks_path {
            advance(state, ext_id);
        }
        let id_key = id_key_for(list_path);
        let found = state
            .store
            .read()
            .expect("store lock")
            .entity(list_path, id_key, ext_id);
        if let Some(entity) = found {
            // One-shot: this answer is honest, and the entity changes right after it, so the
            // conditional mutation the caller is about to send is a 412.
            let mut pending = state.mutate_between.lock().expect("race lock");
            if let Some(i) = pending
                .iter()
                .position(|(p, e)| p == list_path && e == ext_id)
            {
                pending.remove(i);
                let mut bumped = entity.clone();
                // Move a timestamp the entity already carries, so the row keeps its real
                // shape; a nonce only when it has none to move.
                match ["updateTime", "lastUpdatedTime"]
                    .into_iter()
                    .find(|k| bumped.get(k).is_some())
                {
                    Some(k) => bumped[k] = json!(DONE_AT),
                    None => bumped["$mockNonce"] = json!(uuid::Uuid::new_v4().to_string()),
                }
                state
                    .store
                    .write()
                    .expect("store lock")
                    .replace(list_path, id_key, ext_id, bumped);
            }
            return (
                StatusCode::OK,
                [(header::ETAG, etag_of(&entity))],
                Json(json!({"data": entity, "metadata": {"flags": [], "links": []}})),
            )
                .into_response();
        }
        if state.store.read().expect("store lock").has(list_path) {
            return error(StatusCode::NOT_FOUND, &format!("no entity {ext_id}"));
        }
    }
    if nutsh_catalog::KINDS
        .iter()
        .any(|k| same_shape(k.list_path, api_path))
    {
        return list_response(&[], page, limit, state.miscounts.get(api_path).copied());
    }
    error(StatusCode::NOT_FOUND, &format!("unknown path {api_path}"))
}

/// `$page` (from 0) and `$limit` (1..=100, default 50) as Prism validates them: out of range
/// or unparseable is a validation error, not something to clamp.
fn paging(query: &[(String, String)]) -> Result<(usize, usize), &'static str> {
    let raw = |key: &str| {
        query
            .iter()
            .find(|(q, _)| q == key)
            .map(|(_, v)| v.as_str())
    };
    let page = match raw("$page") {
        None => 0,
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| "$page must be a non-negative integer")?,
    };
    let limit = match raw("$limit") {
        None => 50,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if (1..=100).contains(&n) => n,
            _ => return Err("$limit must be between 1 and 100"),
        },
    };
    Ok((page, limit))
}

/// Applies whatever [`crate::MockPc::mutate_after_list`] armed for this list path, counting
/// this answer against each one-shot and firing the ones that reach zero. Off the hot path in
/// every other test: the queue is empty, so this is one uncontended lock.
///
/// An ext id no row carries is a test that armed something it did not mean, so it panics rather
/// than change nothing quietly and leave the assertion to fail somewhere else.
fn touch_after_list(state: &Shared, api_path: &str) {
    let mut due: Vec<(String, String, String)> = Vec::new();
    {
        let mut pending = state.mutate_after_list.lock().expect("touch lock");
        pending.retain_mut(|t| {
            if t.path != api_path {
                return true;
            }
            t.answers = t.answers.saturating_sub(1);
            if t.answers > 0 {
                return true;
            }
            due.push((t.ext_id.clone(), t.field.clone(), t.value.clone()));
            false
        });
    }
    if due.is_empty() {
        return;
    }
    let id_key = id_key_for(api_path);
    let mut store = state.store.write().expect("store lock");
    for (ext_id, field, value) in due {
        let Some(mut entity) = store.entity(api_path, id_key, &ext_id) else {
            panic!("mutate_after_list: no entity {ext_id} in {api_path}");
        };
        entity[field] = json!(value);
        store.replace(api_path, id_key, &ext_id, entity);
    }
}

/// The list in the order `$orderby` asked for, when this mock honours it: one top-level
/// property, `asc` (the default) or `desc`. Anything else - several fields, a nested path, no
/// parameter at all - is answered in file order, which is what a Prism Central that does not
/// implement the parameter does.
fn ordered<'a>(items: &'a [Value], query: &[(String, String)]) -> Cow<'a, [Value]> {
    let Some(spec) = query
        .iter()
        .find(|(k, _)| k == "$orderby")
        .map(|(_, v)| v.as_str())
    else {
        return Cow::Borrowed(items);
    };
    let mut words = spec.split_whitespace();
    let (Some(field), direction) = (words.next(), words.next()) else {
        return Cow::Borrowed(items);
    };
    if words.next().is_some() || field.contains([',', '/']) {
        return Cow::Borrowed(items);
    }
    let descending = direction.is_some_and(|d| d.eq_ignore_ascii_case("desc"));
    let mut sorted = items.to_vec();
    // Stable, so a page of equal keys is the same page every run.
    sorted.sort_by(
        |a, b| match (sort_key(a.get(field)), sort_key(b.get(field))) {
            // A row that does not carry the property sorts last, whichever way the rest goes.
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) if descending => b.cmp(&a),
            (Some(a), Some(b)) => a.cmp(&b),
        },
    );
    Cow::Owned(sorted)
}

/// One value as a sort key. A timestamp's fractional seconds are padded to nine digits, so that
/// `…01.21951Z` and `…01.219511Z` order by time rather than by byte length: Prism trims
/// trailing zeros, and `fixtures-lab/prism/v4.4/config/tasks.json` holds both shapes. Anything
/// else sorts by its own text, which is what a mock owes a test.
fn sort_key(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let Some((head, rest)) = text.split_once('.') else {
        return Some(text.to_string());
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (fraction, tail) = rest.split_at(digits);
    Some(format!("{head}.{fraction:0<9}{tail}"))
}

/// The rows a `$select` asks for and nothing else, the way a real Prism Central answers one.
/// `$objectType` and the kind's own ext-id key come back whatever was asked for, because they
/// always do; everything outside the projection is simply absent, which is what makes a list
/// row different from the document a `GET` by ext id returns.
fn narrowed<'a>(
    items: &'a [Value],
    api_path: &str,
    query: &[(String, String)],
) -> Cow<'a, [Value]> {
    let Some((_, select)) = query.iter().find(|(k, _)| k == "$select") else {
        return Cow::Borrowed(items);
    };
    let id_key = id_key_for(api_path);
    let keep: Vec<&str> = select
        .split(',')
        .map(|f| f.trim().split('.').next().unwrap_or("").trim())
        .filter(|f| !f.is_empty())
        .chain(["$objectType", id_key])
        .collect();
    Cow::Owned(
        items
            .iter()
            .map(|item| match item.as_object() {
                None => item.clone(),
                Some(o) => Value::Object(
                    o.iter()
                        .filter(|(k, _)| keep.contains(&k.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
            })
            .collect::<Vec<_>>(),
    )
}

fn list_response(items: &[Value], page: usize, limit: usize, miscount: Option<u64>) -> Response {
    let start = page.saturating_mul(limit).min(items.len());
    let end = start.saturating_add(limit).min(items.len());
    let total = miscount.unwrap_or_else(|| u64::try_from(items.len()).unwrap_or(u64::MAX));
    Json(json!({
        "data": &items[start..end],
        "metadata": {"totalAvailableResults": total, "flags": [], "links": []}
    }))
    .into_response()
}

/// The `ext_id_key` of the kind whose list path matches, `"extId"` when no kind does.
pub(crate) fn id_key_for(list_path: &str) -> &'static str {
    nutsh_catalog::KINDS
        .iter()
        .find(|k| same_shape(k.list_path, list_path))
        .map_or("extId", |k| k.ext_id_key)
}

/// The catalog action behind a request, with the kind that owns it. The method matters:
/// `update` and `delete` share the entity path.
fn catalog_action_of(
    method: &Method,
    api_path: &str,
) -> Option<(&'static nutsh_catalog::Kind, &'static nutsh_catalog::Action)> {
    nutsh_catalog::KINDS.iter().find_map(|k| {
        k.actions
            .iter()
            .find(|a| a.method.as_str() == method.as_str() && same_shape(a.path, api_path))
            .map(|a| (k, a))
    })
}

/// One step of a mock-created task, applied on a GET by extId before the read: the first GET
/// shows the state the mutation left, each later one the next step. A list GET advances
/// nothing and fixture tasks never advance, so a Tasks table does not race the watcher. Every
/// GET by extId is a step, including the ETag pre-fetch a client makes before `cancel` while
/// the catalog marks it `needs_etag`: a task one GET from terminal cannot be cancelled.
fn advance(state: &Shared, ext_id: &str) {
    let mut tasks = state.tasks.lock().expect("tasks lock");
    let Some(t) = tasks.get_mut(ext_id) else {
        return;
    };
    let tasks_path = state.tasks_path.as_str();
    let mut store = state.store.write().expect("store lock");
    let Some(mut entity) = store.entity(tasks_path, "extId", ext_id) else {
        return;
    };
    if t.canceling {
        // Two GETs: `CANCELING`, then `CANCELED`. Progress unchanged: a cancelled task stops
        // where it was.
        if entity["status"] == "CANCELING" {
            entity["status"] = json!("CANCELED");
            entity["completedTime"] = json!(DONE_AT);
            entity["lastUpdatedTime"] = json!(DONE_AT);
            entity["isCancelable"] = json!(false);
            tasks.remove(ext_id);
        } else {
            entity["status"] = json!("CANCELING");
            entity["lastUpdatedTime"] = json!(step_time(t.step));
        }
        store.replace(tasks_path, "extId", ext_id, entity);
        return;
    }
    if state.stall_task.contains(&t.operation) {
        return;
    }
    let Some(last) = state.task_steps.len().checked_sub(1) else {
        return;
    };
    if t.step > last {
        return;
    }
    let (status, progress) = &state.task_steps[t.step];
    let terminal = t.step == last;
    let failing = state.fail_task.contains(&t.operation);
    let status = if terminal && failing {
        "FAILED"
    } else {
        status.as_str()
    };
    let at = if terminal {
        DONE_AT.to_string()
    } else {
        step_time(t.step)
    };
    entity["status"] = json!(status);
    entity["progressPercentage"] = json!(progress);
    entity["lastUpdatedTime"] = json!(at);
    if status != "QUEUED" && entity.get("startedTime").is_none() {
        entity["startedTime"] = json!(at);
    }
    if terminal {
        entity["completedTime"] = json!(DONE_AT);
        entity["isCancelable"] = json!(false);
        if failing {
            entity["errorMessages"] = json!([{
                "message": format!("{} failed in the mock", t.operation),
                "severity": "ERROR",
                "code": "MOCK-1"
            }]);
        } else {
            apply_effect(&mut store, t);
        }
    }
    t.step += 1;
    store.replace(tasks_path, "extId", ext_id, entity);
}

/// What a successful task did to the entity. The curated power actions move `powerState` and
/// `delete` removes the row; everything else leaves the entity alone, which is honest: the mock
/// is not a hypervisor.
fn apply_effect(store: &mut Store, t: &MockTask) {
    let Some(Target {
        list_path,
        id_key,
        ext_id,
    }) = &t.target
    else {
        return;
    };
    let power = match t.operation.as_str() {
        "power-on" | "power-cycle" | "reset" | "reboot" | "guest-reboot" => "ON",
        "power-off" | "shutdown" | "guest-shutdown" => "OFF",
        "delete" => {
            store.remove(list_path, id_key, ext_id);
            return;
        }
        _ => return,
    };
    if let Some(mut e) = store.entity(list_path, id_key, ext_id) {
        e["powerState"] = json!(power);
        store.replace(list_path, id_key, ext_id, e);
    }
}

/// Insert the task an accepted mutation returns. A list-level create has no target: it names
/// the operation alone and affects no entity.
fn start_task(state: &Shared, operation: &str, target: Option<Target>) -> String {
    let task_id = format!("ZXJnb24=:{}", uuid::Uuid::new_v4());
    let affected: Vec<Value> = target
        .iter()
        .map(|t| {
            let name = state
                .store
                .read()
                .expect("store lock")
                .entity(&t.list_path, t.id_key, &t.ext_id)
                .and_then(|e| {
                    nutsh_catalog::NAME_KEYS
                        .iter()
                        .find_map(|k| e.get(*k).and_then(Value::as_str).map(str::to_string))
                })
                .unwrap_or_else(|| t.ext_id.clone());
            json!({"extId": t.ext_id, "rel": "mock", "name": name})
        })
        .collect();
    let description = match affected.first().and_then(|a| a["name"].as_str()) {
        Some(name) => format!("{operation} on {name}"),
        None => operation.to_string(),
    };
    let (status, progress) = state
        .task_steps
        .first()
        .cloned()
        .unwrap_or(("QUEUED".into(), 0));
    let mut entity = json!({
        "$objectType": "prism.v4.config.Task",
        "extId": task_id,
        "operation": operation,
        "operationDescription": description,
        "status": status,
        "progressPercentage": progress,
        "createdTime": CREATED_AT,
        "lastUpdatedTime": CREATED_AT,
        "isCancelable": true,
        "numberOfSubtasks": 0,
        "numberOfEntitiesAffected": affected.len(),
        "entitiesAffected": affected,
        "subSteps": []
    });
    // A queued task has not started; `advance` stamps the start on the first step that has.
    if status != "QUEUED" {
        entity["startedTime"] = json!(CREATED_AT);
    }
    state
        .store
        .write()
        .expect("store lock")
        .push(&state.tasks_path, entity);
    state.tasks.lock().expect("tasks lock").insert(
        task_id.clone(),
        MockTask {
            step: 0,
            operation: operation.to_string(),
            target,
            canceling: false,
        },
    );
    task_id
}

fn mutate(
    state: &Shared,
    method: &Method,
    api_path: &str,
    headers: &HeaderMap,
    body: Option<&Value>,
) -> Response {
    if headers.get("ntnx-request-id").is_none() {
        return error(
            StatusCode::BAD_REQUEST,
            "NTNX-Request-Id header is required",
        );
    }
    let found = catalog_action_of(method, api_path);
    if let Some((kind, action)) = found
        && state
            .forbid_action
            .iter()
            .any(|(k, a)| k == kind.id && a == action.name)
    {
        return error(
            StatusCode::FORBIDDEN,
            &format!(
                "{} on {} is not permitted for this account",
                action.name, kind.id
            ),
        );
    }
    let entity_path = api_path.split("/$actions/").next().unwrap_or(api_path);
    let is_entity_op = *method != Method::POST || api_path.contains("/$actions/");
    let target = if is_entity_op {
        let Some((list_path, ext_id)) = entity_path.rsplit_once('/') else {
            return error(StatusCode::NOT_FOUND, "bad path");
        };
        // Host maintenance lives under `/operations/clusters/{c}/hosts/{h}` while the entity is
        // recorded under `/config/clusters/{c}/hosts`: one entity, two path families. The
        // fixture tree is keyed by the config paths, so an `/operations/` action looks there.
        let list_path = if state.store.read().expect("store lock").has(list_path) {
            list_path.to_string()
        } else {
            list_path.replacen("/operations/", "/config/", 1)
        };
        let id_key = id_key_for(&list_path);
        let Some(entity) = state
            .store
            .read()
            .expect("store lock")
            .entity(&list_path, id_key, ext_id)
        else {
            return error(StatusCode::NOT_FOUND, &format!("no entity {ext_id}"));
        };
        // The catalog decides. An entity operation with no catalog action behind it keeps the
        // strict behaviour; an action that declares no If-Match rejects one that is sent,
        // which is the 400 a real Prism Central returns.
        let wants_etag = found.is_none_or(|(_, a)| a.needs_etag);
        let sent = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok());
        match (wants_etag, sent) {
            (true, None) => return error(StatusCode::BAD_REQUEST, "If-Match header is required"),
            (true, Some(tag)) if tag != etag_of(&entity) => {
                return error(StatusCode::PRECONDITION_FAILED, "ETag mismatch");
            }
            (false, Some(_)) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "no If-Match expected for this action",
                );
            }
            _ => {}
        }
        Some(Target {
            list_path,
            id_key,
            ext_id: ext_id.to_string(),
        })
    } else if !state.store.read().expect("store lock").has(api_path)
        && !nutsh_catalog::KINDS
            .iter()
            .any(|k| same_shape(k.list_path, api_path))
    {
        return error(StatusCode::NOT_FOUND, &format!("unknown path {api_path}"));
    } else {
        None
    };
    if api_path.contains("/$actions/")
        && let Some((_, action)) = found
    {
        // Two flags, not one: a body is refused where the catalog declares none and required
        // only where the spec says `required: true`; `clone` takes one it does not need.
        match (action.needs_body, action.takes_body, body.is_some()) {
            (_, false, true) => {
                return error(StatusCode::BAD_REQUEST, "no body expected for this action");
            }
            (true, _, false) => return error(StatusCode::BAD_REQUEST, "request body required"),
            _ => {}
        }
    }
    // Cancel is not a task of its own: it flips the task it names, and answers an AppMessage.
    // A fixture task is cancelable too - it just has no `MockTask` yet, so one is registered on
    // the spot; without that, `:tasks` + `c` could never be exercised against the mock.
    if let Some(rest) = api_path.strip_prefix(state.tasks_path.as_str())
        && let Some(rest) = rest.strip_prefix('/')
        && let Some(ext_id) = rest.strip_suffix("/$actions/cancel")
    {
        let entity =
            state
                .store
                .read()
                .expect("store lock")
                .entity(&state.tasks_path, "extId", ext_id);
        let cancelable = entity.as_ref().is_some_and(|e| {
            e.get("isCancelable").and_then(Value::as_bool) == Some(true)
                && !matches!(
                    e.get("status").and_then(Value::as_str).unwrap_or(""),
                    "SUCCEEDED" | "FAILED" | "CANCELING" | "CANCELED"
                )
        });
        let Some(entity) = entity.filter(|_| cancelable) else {
            return error(StatusCode::BAD_REQUEST, "task is not cancelable");
        };
        let mut tasks = state.tasks.lock().expect("tasks lock");
        let t = tasks.entry(ext_id.to_string()).or_insert_with(|| MockTask {
            step: 0,
            operation: entity["operation"].as_str().unwrap_or("").to_string(),
            target: None,
            canceling: false,
        });
        if t.canceling {
            return error(StatusCode::BAD_REQUEST, "task is not cancelable");
        }
        t.canceling = true;
        return (
            StatusCode::OK,
            Json(json!({
                "data": {"$objectType": "prism.v4.error.AppMessage", "message": "cancellation requested"},
                "metadata": {"flags": [], "links": []}
            })),
        )
            .into_response();
    }
    let operation = found.map_or_else(
        || method.to_string().to_ascii_lowercase(),
        |(_, a)| a.name.to_string(),
    );
    let task = start_task(state, &operation, target);
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "data": {"$objectType": "prism.v4.config.TaskReference", "extId": task},
            "metadata": {"flags": [], "links": []}
        })),
    )
        .into_response()
}

/// Segment-wise match where a `{...}` catalog segment stands for any one request segment.
fn same_shape(catalog_path: &str, api_path: &str) -> bool {
    let catalog: Vec<&str> = catalog_path.split('/').collect();
    let api: Vec<&str> = api_path.split('/').collect();
    catalog.len() == api.len()
        && catalog
            .iter()
            .zip(&api)
            .all(|(c, a)| c.starts_with('{') || c == a)
}

fn error_body(status: StatusCode, message: &str) -> Value {
    json!({
        "data": {
            "$objectType": "prism.v4.error.ErrorResponse",
            "error": [{"message": message, "severity": "ERROR", "code": status.as_u16().to_string()}]
        },
        "metadata": {"flags": [], "links": []}
    })
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(error_body(status, message))).into_response()
}
