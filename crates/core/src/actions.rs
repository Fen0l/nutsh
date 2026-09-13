//! One path from a row to a mutation. `plan` resolves what to send, `reason` says why it
//! cannot run, `build_body` turns a form into a request body, `execute` sends it.
//!
//! `execute` touches neither the journal nor the scheduler - the caller owns both - so a test
//! can drive it without either.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nutsh_catalog::{Action, Field, FieldType, Kind};
use nutsh_prism::{ActionResult, Client, Entity, PrismError, TaskRef};
use serde_json::{Map, Value};

use crate::can_i::CanIndex;
use crate::guardrails::{Guardrails, Verdict};
use crate::session::Session;
use crate::store::Failure;

#[derive(Debug, Clone)]
pub struct Plan {
    /// The kind the request runs against: the row's, or its `action_kind`.
    pub kind: &'static Kind,
    pub ext_id: String,
    pub name: String,
    pub action: &'static Action,
    pub body: Option<Value>,
    pub parents: Vec<String>,
    /// The row's own kind, which is what a refresh has to be issued against.
    pub row_kind: &'static Kind,
}

/// The part of a plan the UI needs, and no body: what travels on the poll channel.
#[derive(Debug, Clone)]
pub struct PlanRef {
    pub kind: &'static Kind,
    pub ext_id: String,
    pub name: String,
    pub action: &'static Action,
    /// The row's own kind, carried because a refresh and a task watch are issued against the
    /// row's table, not against the kind the request ran on. An `&'static Kind` costs nothing
    /// to carry and it is the one thing the receiver cannot work out again.
    pub row_kind: &'static Kind,
}

impl Plan {
    /// `to_`, not `as_`: this allocates two `String`s. `as_ref` would promise a borrow and be
    /// reached for inside a loop.
    pub fn to_ref(&self) -> PlanRef {
        PlanRef {
            kind: self.kind,
            ext_id: self.ext_id.clone(),
            name: self.name.clone(),
            action: self.action,
            row_kind: self.row_kind,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Started(TaskRef),
    Done,
    Returned(Value),
    /// `forbidden` is the server's 403 and nothing else. It travels as a flag rather than as a
    /// sentence to be recognised again downstream: a caller that acts on a 403 - the can-i
    /// answer it disproves - is then linked to [`PrismError::is_forbidden`] by the compiler,
    /// and rewording the error's `Display` cannot quietly disable it.
    Failed {
        /// Typed, so the status line and the journal can want different things from it: the
        /// screen draws `Failure::text`, and `:journal` keeps `Failure::upstream`, which is
        /// where the far end's own sentence belongs and the only place it appears.
        failure: Failure,
        forbidden: bool,
    },
}

impl Outcome {
    /// A failure this client decided on rather than one the server answered with, so never a
    /// 403: a body that is missing, a plan that could not be built.
    pub fn failed(message: impl Into<String>) -> Outcome {
        Outcome::Failed {
            failure: Failure::local(message),
            forbidden: false,
        }
    }
}

/// The one wording for a row whose leading ids neither the table nor the row itself carries.
/// `plan` and `reason` must give the same answer, so they share the string.
const NO_PARENT: &str = "needs a parent this table cannot supply";

/// Likewise for the two paths that refuse a read-only session.
const READ_ONLY: &str = "read-only session";

/// The kind whose rows carry `isCancelable`, and the refusal read off it.
const TASK_KIND: &str = "prism.config.Task";
const NOT_CANCELABLE: &str = "not cancelable";

/// Resolve `action_kind` and `action_parents`: the ids a table cannot supply are read out of
/// the row.
pub fn plan(
    kind: &'static Kind,
    entity: &Entity,
    action: &'static Action,
    table_parents: &[String],
    body: Option<Value>,
) -> Result<Plan, String> {
    let target = kind.action_target();
    let mut parents = table_parents.to_vec();
    for path in kind.action_parents {
        let value = crate::path::first(&entity.raw, path)
            .and_then(Value::as_str)
            .ok_or_else(|| NO_PARENT.to_string())?;
        parents.push(value.to_string());
    }
    if parents.len() != action.parents_needed() {
        return Err(NO_PARENT.to_string());
    }
    Ok(Plan {
        kind: target,
        ext_id: entity.ext_id.clone(),
        name: entity.name.clone(),
        action,
        body,
        parents,
        row_kind: kind,
    })
}

pub async fn execute(client: &Client, plan: &Plan) -> Outcome {
    let result = if plan.action.name == "update" {
        // `Client::update` merges the change set over the entity it read and PUTs the result,
        // so nothing to merge means the entity is PUT back verbatim: a task that burns a slot,
        // settles as `Succeeded`, and changed nothing. Its own guard cannot catch this - an
        // empty object *is* an object - so the refusal belongs here, and manufacturing a `{}`
        // for an absent body would walk straight into it.
        let Some(body) = plan.body.clone().filter(|b| !is_empty_object(b)) else {
            return Outcome::failed("update needs a request body");
        };
        client
            .update(plan.kind, &plan.parents, &plan.ext_id, body)
            .await
    } else {
        client
            .act(
                plan.kind,
                &plan.parents,
                &plan.ext_id,
                plan.action.name,
                plan.body.clone(),
            )
            .await
    };
    match result {
        Ok(ActionResult::Task(t)) => Outcome::Started(t),
        Ok(ActionResult::Payload(v)) => Outcome::Returned(v),
        Ok(ActionResult::Empty) => Outcome::Done,
        Err(PrismError::Conflict { .. }) => Outcome::failed("changed since you loaded it"),
        Err(e) => Outcome::Failed {
            forbidden: e.is_forbidden(),
            failure: Failure::of(plan.kind, &e, false),
        },
    }
}

/// A body with nothing in it to merge. A non-object is not this function's business:
/// `Client::update` refuses one with the wording that names the offending type.
fn is_empty_object(body: &Value) -> bool {
    body.as_object().is_some_and(Map::is_empty)
}

/// Everything about the session a refusal can depend on: who is connected, the local rules,
/// and what this account may do. One borrow group rather than three parameters, so the menu,
/// a direct key and `:can-i` all ask the question the same way.
///
/// Named for what it decides. `Context` was rejected: `session.context` is the *connection*
/// context, it is read three lines into `reason`, and one word with two meanings in one
/// function is how a call site ends up passing the wrong one.
#[derive(Debug, Clone, Copy)]
pub struct Policy<'a> {
    pub session: &'a Session,
    pub guardrails: &'a Guardrails,
    pub can_i: &'a CanIndex,
}

/// Why this action cannot run on this row, or `None` when it can. The one place the reasons are
/// composed, so the menu, a direct key and `:can-i` can never disagree.
///
/// Read-only comes first because it is the strongest and cheapest; availability and parents come
/// before the guardrails because a rule about an action this session cannot reach at all is
/// noise. `count` is the mark count, so the bulk cap is answered by the same call.
pub fn reason(
    policy: Policy<'_>,
    kind: &'static Kind,
    action: &'static Action,
    table_parents: &[String],
    entity: Option<&Entity>,
    count: usize,
) -> Option<String> {
    if policy.session.readonly || policy.guardrails.readonly {
        return Some(READ_ONLY.to_string());
    }
    let target = kind.action_target();
    // The rule lives on `Availability` itself, so a greyed menu row, a greyed palette row and
    // a refused action cannot drift apart into three spellings of one refusal.
    if let Some(reason) = policy.session.client.availability(target).reason() {
        return Some(reason);
    }
    // A task that says it is not cancelable would be answered 400, and the row already says so.
    // The refusal lives here rather than at the key, so the menu greys the row with it, `:can-i`
    // agrees with both, and the attempt is journalled like every other refusal. Keyed on the
    // kind, not on the action's name alone: a `cancel` on some later kind must not be refused
    // by a field that kind does not carry.
    if kind.id == TASK_KIND && action.name == "cancel" && !cancelable(entity) {
        return Some(NOT_CANCELABLE.to_string());
    }
    let supplied = table_parents.len()
        + kind
            .action_parents
            .iter()
            .filter(|p| {
                entity.is_some_and(|e| {
                    crate::path::first(&e.raw, p)
                        .and_then(Value::as_str)
                        .is_some()
                })
            })
            .count();
    if supplied != action.parents_needed() {
        return Some(NO_PARENT.to_string());
    }
    if action.needs_body && action.form.is_empty() && action.body.is_none() {
        return Some("needs a request body this catalog cannot describe".to_string());
    }
    let context = policy.session.context.as_deref().unwrap_or("(env)");
    match policy
        .guardrails
        .verdict(context, kind, action, count, &policy.can_i.can(action))
    {
        // Rule 1 above already answered this one; the arm exists because a `Verdict` is a
        // value, not a promise about the order its producer was called in.
        Verdict::ReadOnly => Some(READ_ONLY.to_string()),
        // A rule that gives no `reason` still has to say something.
        Verdict::Deny(None) => Some("denied by a guardrail".to_string()),
        Verdict::Deny(Some(reason)) => Some(format!("denied: {reason}")),
        Verdict::NotPermitted(roles) => Some(format!("not permitted: needs {}", roles.join(", "))),
        Verdict::TooMany { max } => Some(format!(
            "{count} marked, this guardrail allows {max} at a time"
        )),
        Verdict::Confirm(_) | Verdict::Allow => None,
    }
}

/// Whether a task row may be cancelled. No row at all is not a refusal: the question is then
/// about the action and the kind - `:can-i cancel tasks` with the cursor on nothing - and not
/// about a row that does not exist.
fn cancelable(entity: Option<&Entity>) -> bool {
    entity.is_none_or(|e| e.raw.get("isCancelable").and_then(Value::as_bool) == Some(true))
}

/// The fields plus what the form collected, as a request body. The single place substitutions
/// happen: generation cannot do them, because `$now` is not a constant.
pub fn build_body(
    fields: &[Field],
    entered: &HashMap<&str, String>,
    entity: &Entity,
    now: SystemTime,
) -> Result<Value, String> {
    let mut body = Value::Object(Map::new());
    for f in fields {
        let raw = match entered.get(f.name).filter(|v| !v.is_empty()) {
            Some(v) => v.clone(),
            None => match f.value {
                Some(v) => expand(v, entity, now)?,
                None if f.required => {
                    return Err(format!("{} is required", label_of(f)));
                }
                None => continue,
            },
        };
        let value = match f.ty {
            FieldType::Bool => Value::Bool(raw == "true" || raw == "yes"),
            FieldType::Integer => raw
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| format!("{} must be a whole number", label_of(f)))?,
            FieldType::Json(_) => {
                serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", label_of(f)))?
            }
            // A reference's body path is the object, not the id inside it: the generator calls
            // a property a `Reference` when its schema is an object carrying `extId` or `uuid`
            // (`xtask/src/catalog/parse.rs`), and `ClusterReference` is `type: object,
            // required: [extId], additionalProperties: false`. A bare string there is a wrong
            // body, and on an update it would go out over the object the Prism Central has.
            //
            // Two shapes reach here and both have to leave as the object. A prefill holds the
            // reference's own wire form, already carrying whichever key that schema uses, so it
            // is passed through untouched. A picker leaves the bare ext id it chose, and that is
            // what gets wrapped.
            // The generator writes a reference either way round: `cluster`, addressing the
            // object, or `rack.extId`, addressing the id inside it. The last segment says
            // which, the same `.extId`/`.uuid` suffix the column de-duplication reads.
            FieldType::Reference(_) if is_id_path(f.name) => Value::String(raw),
            FieldType::Reference(_) => match serde_json::from_str::<Value>(&raw) {
                Ok(object @ Value::Object(_)) => object,
                _ => {
                    let mut reference = Map::new();
                    reference.insert("extId".to_string(), Value::String(raw));
                    Value::Object(reference)
                }
            },
            _ => Value::String(raw),
        };
        write_at(&mut body, f.name, value)?;
    }
    Ok(body)
}

/// Whether a reference field's path addresses the id inside the reference rather than the
/// reference itself: `rack.extId` and `host.cluster.uuid` do, `cluster` and `targetCluster`
/// do not. The ones that do are already a string on the wire and stay one.
fn is_id_path(name: &str) -> bool {
    matches!(name.rsplit('.').next(), Some("extId" | "uuid"))
}

/// What a field is called: the curated label, or the body path it writes when the generator
/// derived none. Public because the form draws its rows with it, so a refusal from
/// [`build_body`] and the row it is about can never name the same field differently.
pub fn label_of(f: &Field) -> &str {
    if f.label.is_empty() { f.name } else { f.label }
}

/// `$ext_id` and `$now+<n>d`; anything else starting with `$` is an overlay typo.
fn expand(value: &str, entity: &Entity, now: SystemTime) -> Result<String, String> {
    if !value.starts_with('$') {
        return Ok(value.to_string());
    }
    if value == "$ext_id" {
        return Ok(entity.ext_id.clone());
    }
    if let Some(days) = value
        .strip_prefix("$now+")
        .and_then(|d| d.strip_suffix('d'))
        .and_then(|d| d.parse::<u64>().ok())
    {
        // A null expirationTime is an HTTP 400, so this must always produce one.
        // Checked the whole way: `days` comes from `curated.toml`, and both the multiply and
        // `SystemTime::add` panic on overflow. Every other malformed substitution here is an
        // `Err` the form shows; this one must not be the exception that aborts the process.
        return days
            .checked_mul(86_400)
            .and_then(|secs| now.checked_add(Duration::from_secs(secs)))
            .map(rfc3339)
            .ok_or_else(|| format!("substitution {value} is out of range"));
    }
    Err(format!("unknown substitution {value}"))
}

fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let dt = time::OffsetDateTime::from_unix_timestamp(secs as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    // Spelled out rather than `time`'s `Rfc3339`, whose formatter needs the crate's
    // `formatting` feature; the whole workspace takes `time` for its parser alone.
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Write `value` at `name`, creating objects and growing arrays with `Value::Null` as it goes.
/// A **write** grammar, deliberately not `crate::path::get`, which reads and fans out over `[]`
/// with no index; the two never share code.
fn write_at(body: &mut Value, name: &str, value: Value) -> Result<(), String> {
    if !nutsh_catalog::valid_field_name(name) {
        return Err(format!("field name {name:?} is not a write path"));
    }
    let mut current = body;
    let segments: Vec<&str> = name.split('.').collect();
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let (key, index) = match segment.split_once('[') {
            None => (*segment, None),
            Some((key, rest)) => (key, rest.trim_end_matches(']').parse::<usize>().ok()),
        };
        let Some(object) = current.as_object_mut() else {
            // The value that is not an object belongs to the segments *before* this one -
            // `current` is what they resolved to - so name the prefix that leads here rather
            // than the key about to be written. `i == 0` cannot happen: `body` starts as one.
            return Err(format!(
                "{name}: {} is not an object",
                segments[..i].join(".")
            ));
        };
        match index {
            None if last => {
                object.insert(key.to_string(), value);
                return Ok(());
            }
            None => {
                current = object
                    .entry(key.to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
            }
            Some(n) => {
                let array = object
                    .entry(key.to_string())
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .ok_or_else(|| format!("{name}: {key} is not an array"))?;
                while array.len() <= n {
                    array.push(Value::Null);
                }
                if last {
                    array[n] = value;
                    return Ok(());
                }
                if !array[n].is_object() {
                    array[n] = Value::Object(Map::new());
                }
                current = &mut array[n];
            }
        }
    }
    Ok(())
}

/// What `fields` already are on `entity`, keyed by field name, as the strings the form edits.
///
/// An update edits what is there. A form that starts blank invites a user to clear a field
/// they never meant to touch, and every field they leave alone is sent as an absence - so the
/// form is seeded from the entity, and [`build_body`] writes back exactly what it was given.
///
/// A field the document has no value for is absent from the map rather than present and empty:
/// nothing here invents a value. `null` counts as no value for the same reason.
pub fn current_values(fields: &[Field], entity: &Entity) -> HashMap<&'static str, String> {
    fields
        .iter()
        .filter_map(|f| {
            let value = read_at(&entity.raw, f.name)?;
            Some((f.name, as_text(value)?))
        })
        .collect()
}

/// One value as the string its field is edited as. `None` for `null`, which is not a value.
fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        // Compact, because a field is one line: `to_string` on a `Value` is the wire form, and
        // the wire form is what `build_body` parses back out of a `Json` field.
        other => Some(other.to_string()),
    }
}

/// Read the value at `name`, in the grammar [`write_at`] writes. The read half of that
/// grammar and no more: it resolves one `[n]` per segment and fans out over nothing, so what
/// comes back is the single value a write to the same path would replace.
fn read_at<'a>(document: &'a Value, name: &str) -> Option<&'a Value> {
    if !nutsh_catalog::valid_field_name(name) {
        return None;
    }
    let mut current = document;
    for segment in name.split('.') {
        let (key, index) = match segment.split_once('[') {
            None => (segment, None),
            Some((key, rest)) => (key, rest.trim_end_matches(']').parse::<usize>().ok()),
        };
        current = current.as_object()?.get(key)?;
        if let Some(n) = index {
            current = current.as_array()?.get(n)?;
        }
    }
    Some(current)
}
