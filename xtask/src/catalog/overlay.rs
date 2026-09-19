//! `crates/catalog/curated.toml`: hand-maintained display names, aliases, categories,
//! poll intervals, curated columns, and curated actions keyed by kind id.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::model::{
    ActionModel, ColumnModel, DetailSectionModel, FieldKind, FieldModel, KindModel,
};
use super::parse::clear_unfetchable_etags;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    #[serde(default)]
    pub kinds: BTreeMap<String, KindOverlay>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindOverlay {
    pub display: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub category: Option<String>,
    pub poll_secs: Option<u32>,
    /// The `$orderby` every list of this kind is sent with, e.g. `createdTime desc`.
    ///
    /// Not validated against the operation's `$orderby` `x-odata-fields` allowlist, though
    /// every curated order does name a field from it. The pinned specs declare those lists -
    /// `prism` v4.4 gives `listTasks` twelve fields including `createdTime`, `startedTime`
    /// and `lastUpdatedTime`; `monitoring` v4.3 gives `listAlerts` nine including
    /// `creationTime` and `severity`, and `listAudits` seven including `creationTime` - and
    /// pc.7.6 answers 200 for each of those orders (measured 2026-09-09). Earlier notes here
    /// said the specs declared almost nothing; that was v4.0/v4.1, not the pinned versions.
    ///
    /// The check is not implemented because the extension is optional (an endpoint that omits
    /// it would have to mean "anything goes") and an order carries a direction and may name
    /// several clauses. Until it exists, `curated.toml` cites the spec line for each order it
    /// sets, and only the endpoint's own `$orderby` support is enforced below.
    pub orderby: Option<String>,
    /// The monotone last-modified field the change-probe orders by, e.g. `lastUpdatedTime`.
    ///
    /// Checked against the **schema** (it must be a declared timestamp) and against the
    /// endpoint (it must take `$orderby`). Only *warned* about against the `$orderby`
    /// `x-odata-fields` allowlist, and not even that when the allowlist is empty: the runtime
    /// check is calibration, which asks the Prism Central instead of the spec.
    pub probe_by: Option<String>,
    /// The row budget of a list walk over this kind: the walk stops there, whatever the
    /// server's total. Zero is refused - a table with no rows is not a budget.
    pub max_rows: Option<u32>,
    #[serde(default)]
    pub columns: Vec<ColumnModel>,
    /// Curated detail sections: `[[kinds."id".detail]]` with an inline `fields = [...]`.
    #[serde(default)]
    pub detail: Vec<DetailSectionModel>,
    /// Which fallback column carries the row's status, overriding the parser's heuristic.
    /// `""` gives the kind none. Refused beside curated `columns`, which the TUI reads instead
    /// of the fallback ones: a curated column says `kind = "Status"` itself.
    pub status_column: Option<String>,
    /// Per-kind status-word overrides: `QUEUED = "Muted"`. Words are normalised the way
    /// `nutsh_catalog::normalize_status` normalises them, so the file may spell one however
    /// the API does.
    #[serde(default)]
    pub status: BTreeMap<String, String>,
    /// Unattached `$actions` paths this kind claims (§4.3). A path no spec declares fails
    /// generation, as an unknown kind id already does.
    #[serde(default)]
    pub extra_actions: Vec<String>,
    /// Curated actions, keyed by action name. A key that names no action of this kind and
    /// carries no `from` fails generation.
    #[serde(default)]
    pub actions: BTreeMap<String, ActionOverlay>,
    /// Borrow another kind's actions (§4.3).
    pub actions_from: Option<String>,
    /// Dotted paths read out of a row to fill the borrowed path's leading placeholders.
    #[serde(default)]
    pub action_parents: Vec<String>,
    /// The property that identifies an entity of this kind; `extId` when absent.
    pub ext_id_key: Option<String>,
    /// Dotted path to the entity's display name, when `NAME_KEYS` cannot find it at the top
    /// level: `name_path = "config.name"` on `prism.config.DomainManager`.
    pub name_path: Option<String>,
    /// List one page of this kind at connect so its names are in the cache before anything
    /// opens it. Checked by [`check`].
    #[serde(default)]
    pub warm: bool,
    /// The schema this kind's entities have, when the generator could not infer one from the
    /// list response - `microseg.config.policies` is the one that matters, and its schema is
    /// right there as `microseg.v4.3.config.NetworkSecurityPolicy`. Written **without** the
    /// version segment and matched against the pinned spec's keys with that segment stripped.
    pub schema: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionOverlay {
    /// Copy another action instead of curating this one: a bare name from the same kind, or
    /// `"<kind id>:<action>"` from another. It exists because `manage-alert` is two verbs
    /// behind one endpoint.
    pub from: Option<String>,
    pub key: Option<String>,
    pub label: Option<String>,
    /// `none` | `low` | `medium` | `high`.
    pub danger: Option<String>,
    /// `none` | `yes` | `type-name`; defaults from `danger`.
    pub confirm: Option<String>,
    pub order: Option<u16>,
    #[serde(default)]
    pub hidden: bool,
    /// A constant request body.
    pub body: Option<toml::Value>,
    #[serde(default)]
    pub form: Vec<FieldOverlay>,
    /// The one derived field the overlay may override (§5.4). Every override carries its
    /// verification date in a comment.
    pub needs_etag: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldOverlay {
    pub name: String,
    #[serde(default)]
    pub label: String,
    /// `string` | `bool` | `integer` | `enum` | `reference` | `json`; the generated field's
    /// own type when absent.
    #[serde(rename = "type")]
    pub ty: Option<String>,
    #[serde(default)]
    pub values: Vec<String>,
    /// The kind a `reference` field picks from. One the catalog does not have fails
    /// generation; none at all makes the field a plain extId box.
    pub kind: Option<String>,
    /// The generated field's own requirement when absent.
    pub required: Option<bool>,
    pub value: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

/// The entries of `nutsh_catalog::RESERVED_KEYS` that this spec's own curated actions bind.
/// The const is a registry of every key the program binds, navigation and action alike, so the
/// generation check refuses the navigation half only. `c` carries a further rule: only an
/// action literally named `cancel` may claim it, so "cancel" means the same thing on every
/// table while `prism.config.Task` can still bind it.
pub const ACTION_KEYS: &[&str] = &["p", "P", "r", "s", "m", "ctrl-d", "c"];

const DANGERS: &[(&str, &str)] = &[
    ("none", "None"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
];
const CONFIRMS: &[(&str, &str)] = &[("none", "None"), ("yes", "Yes"), ("type-name", "TypeName")];
/// The `type` names a curated field may carry.
const FIELD_TYPES: &[&str] = &["string", "bool", "integer", "enum", "reference", "json"];

/// A `danger` with no explicit `confirm`: safe things run, a medium one asks, a high one asks
/// for the name.
fn confirm_floor(danger: &str) -> &'static str {
    match danger {
        "Medium" => "Yes",
        "High" => "TypeName",
        _ => "None",
    }
}

/// TOML table → the JSON text a constant body is sent as.
fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        toml::Value::String(s) => J::String(s.clone()),
        toml::Value::Integer(i) => J::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
        toml::Value::Boolean(b) => J::Bool(*b),
        toml::Value::Datetime(d) => J::String(d.to_string()),
        toml::Value::Array(a) => J::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => J::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

pub fn parse(text: &str) -> Result<Overlay> {
    Ok(toml::from_str(text)?)
}

/// Move each `extra_actions` path out of `unattached` onto the kind that claims it. All
/// problems are reported at once, as `apply` does. Runs before `apply` so a curated `actions`
/// entry can curate an action this pass created.
pub fn claim_extra_actions(
    overlay: &Overlay,
    kinds: &mut [KindModel],
    unattached: &[ActionModel],
) -> Result<()> {
    let mut problems = Vec::new();
    for (id, o) in &overlay.kinds {
        if o.extra_actions.is_empty() {
            continue;
        }
        let Some(k) = kinds.iter_mut().find(|k| &k.id == id) else {
            problems.push(format!("unknown kind id {id}"));
            continue;
        };
        for path in &o.extra_actions {
            match unattached.iter().find(|a| &a.path == path) {
                Some(a) => k.actions.push(a.clone()),
                None => problems.push(format!(
                    "{id}: extra_actions path {path:?} is not an unattached action of any spec"
                )),
            }
        }
        // A claimed action is scoped by the claiming kind's get path, which is what the ETag
        // fetch fills; a mismatch means it cannot be fetched.
        clear_unfetchable_etags(k);
        k.actions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("curated.toml: {}", problems.join("; "))
    }
}

/// Apply in place. Curated aliases go first. Unknown ids and column kinds are errors so
/// drift is caught at generation time rather than at runtime; all are reported at once.
pub fn apply(overlay: &Overlay, kinds: &mut [KindModel]) -> Result<()> {
    let mut problems = Vec::new();
    for (id, o) in &overlay.kinds {
        let Some(k) = kinds.iter_mut().find(|k| &k.id == id) else {
            problems.push(format!("unknown kind id {id}"));
            continue;
        };
        for c in &o.columns {
            if nutsh_catalog::ColumnKind::from_name(&c.kind).is_none() {
                problems.push(format!(
                    "{id}: unknown column kind {:?} for {}",
                    c.kind, c.path
                ));
            }
        }
        if let Some(d) = &o.display {
            k.display = d.clone();
        }
        if let Some(c) = &o.category {
            k.category = c.clone();
        }
        if let Some(p) = o.poll_secs {
            k.poll_secs = p;
        }
        if let Some(p) = &o.name_path {
            k.name_path = p.clone();
        }
        k.warm = o.warm;
        if let Some(order) = &o.orderby {
            // Checked against the endpoint, never against the spec's `x-odata-fields`: see
            // `KindOverlay::orderby`. An order the endpoint takes no `$orderby` for would be
            // dropped by the client and curated for nothing.
            if order.trim().is_empty() {
                problems.push(format!("{id}: orderby is empty"));
            } else if !k.list_params.orderby {
                problems.push(format!(
                    "{id}: orderby is curated but the list endpoint declares no $orderby"
                ));
            } else {
                k.orderby = Some(order.clone());
            }
        }
        if let Some(field) = &o.probe_by {
            if !k.list_params.orderby {
                problems.push(format!(
                    "{id}: probe_by is curated but the list endpoint declares no $orderby"
                ));
            } else if !k.timestamps.iter().any(|t| t == field) {
                problems.push(format!(
                    "{id}: probe_by {field:?} is not a timestamp the schema declares; a probe \
                     over anything else compares nothing and costs a request a cycle"
                ));
            } else {
                if !k.list_params.orderby_fields.is_empty()
                    && !k.list_params.orderby_fields.iter().any(|f| f == field)
                {
                    eprintln!(
                        "warning: {id}: probe_by {field:?} is not in the endpoint's $orderby \
                         allowlist; calibration will decide at runtime"
                    );
                }
                k.probe_by = Some(field.clone());
            }
        }
        if let Some(rows) = o.max_rows {
            // Zero is not "no budget": every kind gets one, and `generate` fills the default
            // into whatever the overlay leaves unset. A kind that must be walked whole has no
            // way to say so here, and none has yet needed to.
            if rows == 0 {
                problems.push(format!("{id}: max_rows is zero"));
            } else {
                k.max_rows = Some(rows);
            }
        }
        let mut aliases: Vec<String> = Vec::new();
        for a in o.aliases.iter().chain(k.aliases.iter()) {
            if !aliases.contains(a) {
                aliases.push(a.clone());
            }
        }
        k.aliases = aliases;
        k.columns = o.columns.clone();
        k.detail = o.detail.clone();
        k.curated = true;
        if let Some(property) = &o.status_column {
            // The TUI reads curated `columns` and never the fallback ones, so a status column
            // named beside them would validate and colour nothing.
            let problem = if o.columns.is_empty() {
                set_status_column(k, property)
            } else {
                Some(format!(
                    "{id}: status_column has no effect beside curated columns; set kind = \"Status\" on the column instead"
                ))
            };
            problems.extend(problem);
        }
        // Keyed by the normalised word, which is what the runtime looks up: two spellings that
        // fold to one word are a mistake to report, not a coin toss over which survives. The
        // map's order is the sorted order the runtime's binary search wants.
        let mut roles: BTreeMap<String, (String, String)> = BTreeMap::new();
        for (word, role) in &o.status {
            if nutsh_catalog::Role::from_name(role).is_none() {
                problems.push(format!(
                    "{id}: unknown role {role:?} for status {word:?} (known: {})",
                    nutsh_catalog::ROLE_NAMES.join(", ")
                ));
            }
            let normalized = nutsh_catalog::normalize_status(word);
            if let Some((prev, _)) = roles.insert(normalized.clone(), (word.clone(), role.clone()))
            {
                problems.push(format!(
                    "{id}: status words {prev:?} and {word:?} both normalise to {normalized:?}"
                ));
            }
        }
        k.status_roles = roles
            .into_iter()
            .map(|(word, (_, role))| (word, role))
            .collect();
    }
    // Actions are curated after every kind's own fields, in two passes over one overlay: the
    // copies (`from`) first, so a copy can then be curated like any other action.
    for (id, o) in &overlay.kinds {
        let Some(index) = kinds.iter().position(|k| &k.id == id) else {
            continue; // already reported above
        };
        if let Some(key) = &o.ext_id_key {
            kinds[index].ext_id_key = key.clone();
        }
        if let Some(from) = &o.actions_from {
            if kinds.iter().any(|k| &k.id == from) {
                kinds[index].action_kind = Some(from.clone());
            } else {
                problems.push(format!("{id}: actions_from names no kind {from}"));
            }
        }
        kinds[index].action_parents = o.action_parents.clone();
        for (name, a) in &o.actions {
            let Some(source) = &a.from else { continue };
            // A copy over an existing name would leave two actions answering to it, and
            // the second pass would curate whichever came first.
            if kinds[index].actions.iter().any(|x| &x.name == name) {
                problems.push(format!("{id}: from would duplicate action {name}"));
                continue;
            }
            let (source_kind, source_name) = match source.split_once(':') {
                Some((k, n)) => (k, n),
                None => (id.as_str(), source.as_str()),
            };
            let copied = kinds
                .iter()
                .find(|k| k.id == source_kind)
                .and_then(|k| k.actions.iter().find(|x| x.name == source_name))
                .cloned();
            match copied {
                Some(mut c) => {
                    // The copy keeps the source's endpoint, danger and form. Key and label are
                    // its own to curate; `hidden` is set by the second pass, as for any
                    // curated action.
                    c.name = name.clone();
                    c.key = String::new();
                    c.label = String::new();
                    kinds[index].actions.push(c);
                }
                None => problems.push(format!("{id}: from names no action {source}")),
            }
        }
        // A cross-kind copy is scoped by this kind's get path, as a claimed action is: the
        // source's `needs_etag` may want an ETag that path cannot fetch.
        clear_unfetchable_etags(&mut kinds[index]);
        kinds[index].actions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    let ids: Vec<String> = kinds.iter().map(|k| k.id.clone()).collect();
    for (id, o) in &overlay.kinds {
        let Some(index) = kinds.iter().position(|k| &k.id == id) else {
            continue;
        };
        for (name, a) in &o.actions {
            let Some(target) = kinds[index].actions.iter_mut().find(|x| &x.name == name) else {
                // A `from` that landed nothing was reported by the first pass.
                if a.from.is_none() {
                    problems.push(format!("{id}: no action {name}"));
                }
                continue;
            };
            if let Some(label) = &a.label {
                // The menu's narrowest box, as `ACTION_LABEL` derives it. Refused here rather
                // than cut at draw time, for the reason a detail label is: a label the box
                // trims leaves no trace on the frame, so nobody would ever find out.
                let width = label.chars().count();
                if width > nutsh_catalog::ACTION_LABEL {
                    problems.push(format!(
                        "{id}.{name}: action label {label:?} is {width} characters, at most {}",
                        nutsh_catalog::ACTION_LABEL
                    ));
                }
                target.label = label.clone();
            }
            if let Some(order) = a.order {
                target.order = order;
            }
            if let Some(needs_etag) = a.needs_etag {
                target.needs_etag = needs_etag;
            }
            target.hidden = a.hidden;
            if let Some(d) = &a.danger {
                match DANGERS.iter().find(|(from, _)| from == d) {
                    Some((_, to)) => target.danger = (*to).to_string(),
                    None => problems.push(format!(
                        "{id}.{name}: danger {d:?} is not one of none, low, medium, high"
                    )),
                }
            }
            target.confirm = confirm_floor(&target.danger).to_string();
            if let Some(c) = &a.confirm {
                match CONFIRMS.iter().find(|(from, _)| from == c) {
                    Some((_, to)) => target.confirm = (*to).to_string(),
                    None => problems.push(format!(
                        "{id}.{name}: confirm {c:?} is not one of none, yes, type-name"
                    )),
                }
            }
            if let Some(body) = &a.body {
                target.body = Some(toml_to_json(body).to_string());
            }
            if let Some(key) = &a.key {
                // `RESERVED_KEYS` registers every key the program binds; the table's half is
                // the part an action may not claim.
                if nutsh_catalog::RESERVED_KEYS.contains(&key.as_str())
                    && !ACTION_KEYS.contains(&key.as_str())
                {
                    problems.push(format!(
                        "{id}.{name}: key {key:?} is reserved by the table mode"
                    ));
                } else if key == "c" && name != "cancel" {
                    problems.push(format!(
                        "{id}.{name}: key \"c\" is reserved for cancel on every table"
                    ));
                }
                target.key = key.clone();
            }
            for f in &a.form {
                if !nutsh_catalog::valid_field_name(&f.name) {
                    problems.push(format!(
                        "{id}.{name}: field name {:?} is not seg([n])?(.seg([n])?)*",
                        f.name
                    ));
                }
                // A typo here would fall through to the derived type, or open a picker over
                // nothing, at runtime; it fails generation instead, as every curated mistake
                // does.
                match f.ty.as_deref() {
                    None => {}
                    Some(ty) if !FIELD_TYPES.contains(&ty) => problems.push(format!(
                        "{id}.{name}: field {} type {ty:?} is not one of {}",
                        f.name,
                        FIELD_TYPES.join(", ")
                    )),
                    Some("enum") if f.values.is_empty() => problems.push(format!(
                        "{id}.{name}: enum field {} lists no values",
                        f.name
                    )),
                    Some("reference") => {
                        if let Some(kind) =
                            f.kind.as_deref().filter(|k| !ids.iter().any(|i| i == k))
                        {
                            problems.push(format!(
                                "{id}.{name}: reference field {} names no kind {kind}",
                                f.name
                            ));
                        }
                    }
                    Some(_) => {}
                }
            }
            apply_form(&a.form, target);
        }
        // Two actions of one kind may not claim the same key.
        let mut seen: Vec<&str> = Vec::new();
        for a in &kinds[index].actions {
            if a.key.is_empty() {
                continue;
            }
            if seen.contains(&a.key.as_str()) {
                problems.push(format!("{id}: two actions claim key {:?}", a.key));
            }
            seen.push(&a.key);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("curated.toml: {}", problems.join("; "))
    }
}

/// Apply the `schema` overrides and recompute the fallback columns of the kinds that got one.
///
/// It runs after [`apply`] and before [`check`], because `parse` derived those kinds' columns
/// from a schema it could not find - the hard-coded `NAME`/`EXT ID` stub - and the override is
/// exactly the news that there is a schema after all.
pub fn resolve_schemas(
    overlay: &Overlay,
    kinds: &mut [KindModel],
    schemas: &BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    let mut problems = Vec::new();
    for (id, o) in &overlay.kinds {
        let Some(want) = &o.schema else { continue };
        let Some(k) = kinds.iter_mut().find(|k| &k.id == id) else {
            continue; // `apply` already reported the unknown id
        };
        let empty = serde_json::Value::Object(serde_json::Map::new());
        let ns = schemas.get(&k.namespace).unwrap_or(&empty);
        let found: Vec<String> = ns
            .as_object()
            .map(|m| {
                m.keys()
                    .filter(|key| super::parse::strip_version_segments(key) == *want)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        match found.as_slice() {
            [one] => {
                k.schema = one.clone();
                k.fallback_columns = super::parse::fallback_columns(ns, one);
            }
            [] => problems.push(format!(
                "{id}: schema {want:?} matches no schema of namespace {}",
                k.namespace
            )),
            many => problems.push(format!("{id}: schema {want:?} matches {many:?}")),
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("curated.toml: {}", problems.join("; "))
    }
}

/// The checks that need the specs as well as the overlay: [`apply`] has only the kinds, and a
/// dozen tests call it with two arguments, so the schema-aware half lives here and
/// `catalog::generate` runs it straight after `apply`. Every problem is reported, not the
/// first.
///
/// Two questions of every kind - that each curated column's path resolves, and that a curated
/// `Status` sits inside `DEFAULT_COLUMNS` - and two more of a `warm` one: that it can be listed
/// on its own, and that it has a name to cache.
///
/// Six more of a kind that curates a detail, four of them in `detail_shape` - a label that
/// fits `DETAIL_LABEL`, a `kind` the `ColumnKind` vocabulary has, a section with at least one
/// field and at most twenty-four of them inside a kind's twelve, and unique section titles - and
/// two here, which are the two that need the specs: every field path resolves against the kind's
/// schema, and every `when` parses as one of the three forms over a path that does, with a
/// trailing `$objectType` matched against the arm names of the property its prefix names.
///
/// `schemas` is keyed by namespace; each value is that namespace's `components.schemas`
/// object, exactly as `parse` sees it.
pub fn check(kinds: &[KindModel], schemas: &BTreeMap<String, serde_json::Value>) -> Result<()> {
    let empty = serde_json::Value::Object(serde_json::Map::new());
    let mut problems = Vec::new();
    for k in kinds {
        let ns = schemas.get(&k.namespace).unwrap_or(&empty);
        // Every curated column's path resolves against the kind's schema. The overlay
        // otherwise validates that `kind` is a known `ColumnKind` and nothing else, so a typo
        // ships a column of dashes.
        for c in &k.columns {
            if k.schema.is_empty() {
                problems.push(format!(
                    "{}: curated column {:?} but the kind has no schema; add a `schema` override",
                    k.id, c.header
                ));
            } else if !super::parse::resolves(ns, &k.schema, &c.path) {
                problems.push(format!(
                    "{}: column {:?} path {:?} resolves against nothing in {}",
                    k.id, c.header, c.path, k.schema
                ));
            }
        }
        // Checks 2, 3, 4 and 6 first, so a section whose shape is wrong is reported by shape
        // rather than by a path check that cannot run against it.
        problems.extend(detail_shape(&k.id, &k.detail));
        // Every curated detail path resolves, and every `when` is one of the three forms over a
        // path that resolves. Deviation 2: `bootConfig.$objectType` *does* resolve - the
        // `bootConfig` property declares `$objectType` inline beside its `oneOf` - so the
        // `$objectType` rule below is not an exemption from this check but a strengthening of
        // it: it validates the *literal*, which a path check can never do.
        for s in &k.detail {
            for f in &s.fields {
                if !super::parse::resolves(ns, &k.schema, &f.path) {
                    problems.push(format!(
                        "{}: detail field {:?} path {:?} resolves against nothing in {}",
                        k.id, f.label, f.path, k.schema
                    ));
                }
                if let Some(when) = &f.when {
                    problems.extend(check_when(
                        ns,
                        k,
                        when,
                        &format!("detail field {:?}", f.label),
                    ));
                }
            }
            if let Some(when) = &s.when {
                problems.extend(check_when(
                    ns,
                    k,
                    when,
                    &format!("detail section {:?}", s.title),
                ));
            }
        }
        // A curated `Status` below `DEFAULT_COLUMNS`, or it tints nothing.
        for (i, c) in k.columns.iter().enumerate() {
            if c.kind == "Status" && i >= nutsh_catalog::DEFAULT_COLUMNS {
                problems.push(format!(
                    "{}: Status column {:?} is at index {i}; it must be below DEFAULT_COLUMNS ({})",
                    k.id,
                    c.header,
                    nutsh_catalog::DEFAULT_COLUMNS
                ));
            }
        }
        if !k.warm {
            continue;
        }
        // `Reach::Direct` in the catalog's terms, spelled over a `KindModel`: a kind with a
        // parent, an unfilled placeholder, or a required query parameter has no listing of
        // its own for the warm-up to ask for.
        if k.list_params.required || k.parent.is_some() || k.list_path.contains('{') {
            problems.push(format!(
                "{}: warm needs a kind that can be listed on its own",
                k.id
            ));
        }
        // A kind that names itself with its own extId would put `id -> id` in the cache, and
        // the cell would then print 36 characters where it prints an 8-character stub today.
        let names_itself = super::parse::schema_properties(ns, &k.schema)
            .iter()
            .any(|(n, _)| nutsh_catalog::NAME_KEYS.contains(&n.as_str()));
        if k.name_path.is_empty() && !names_itself {
            problems.push(format!(
                "{}: warm needs name_path or a NAME_KEYS property in {}",
                k.id, k.schema
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("curated.toml: {}", problems.join("; "))
    }
}

/// Curated fields in the order the overlay lists them, each keeping whatever the generator
/// derived for it unless the overlay says otherwise. A curated name the schema does not
/// declare is a new field: that is how `vmRecoveryPoints[0].vmExtId` gets in.
fn apply_form(overlay: &[FieldOverlay], target: &mut ActionModel) {
    if overlay.is_empty() {
        return;
    }
    let mut out = Vec::new();
    for f in overlay {
        let derived = target.form.iter().find(|d| d.name == f.name);
        let ty = match f.ty.as_deref() {
            Some("string") => FieldKind::Text,
            Some("bool") => FieldKind::Bool,
            Some("integer") => FieldKind::Integer,
            Some("enum") => FieldKind::Enum(f.values.clone()),
            Some("reference") => FieldKind::Reference(f.kind.clone()),
            Some("json") => FieldKind::Json("{}".to_string()),
            _ => derived.map(|d| d.ty.clone()).unwrap_or(FieldKind::Text),
        };
        out.push(FieldModel {
            name: f.name.clone(),
            label: if f.label.is_empty() {
                derived.map(|d| d.label.clone()).unwrap_or_default()
            } else {
                f.label.clone()
            },
            ty,
            required: f.required.or(derived.map(|d| d.required)).unwrap_or(false),
            value: f.value.clone(),
            hidden: f.hidden,
        });
    }
    target.form = out;
}

/// `status_column` replaces the parser's heuristic: every promoted column goes back to `Enum`,
/// then the named property is promoted instead. An empty name gives the kind no status column
/// at all, which is how a kind whose only enum is noise opts out.
fn set_status_column(k: &mut KindModel, property: &str) -> Option<String> {
    for c in k.fallback_columns.iter_mut() {
        if c.kind == "Status" {
            c.kind = "Enum".to_string();
        }
    }
    if property.is_empty() {
        return None;
    }
    match k.fallback_columns.iter_mut().find(|c| c.path == property) {
        Some(c) => {
            c.kind = "Status".to_string();
            None
        }
        None => Some(format!(
            "{}: status_column {property:?} is not one of its columns",
            k.id
        )),
    }
}

/// The detail checks that need only the overlay: a known column kind (3), a label that fits (2),
/// the section and field counts (4), and unique section titles (6). The two that need the specs -
/// every path resolves (1) and every `when` parses and names a real arm (5) - are inline in
/// [`check`]'s loop, which has them.
///
/// The counts are the numbers past which the pane's layout stops fitting a screen a person will
/// scroll: twelve sections of twenty-four fields is 288 lines at one column.
fn detail_shape(id: &str, sections: &[DetailSectionModel]) -> Vec<String> {
    let mut problems = Vec::new();
    if sections.len() > 12 {
        problems.push(format!(
            "{id}: {} detail sections, at most 12",
            sections.len()
        ));
    }
    let mut titles: Vec<&str> = Vec::new();
    for s in sections {
        if titles.contains(&s.title.as_str()) {
            problems.push(format!("{id}: two detail sections titled {:?}", s.title));
        }
        titles.push(&s.title);
        if s.fields.is_empty() {
            problems.push(format!("{id}: detail section {:?} has no fields", s.title));
        }
        if s.fields.len() > 24 {
            problems.push(format!(
                "{id}: detail section {:?} has {} fields, at most 24",
                s.title,
                s.fields.len()
            ));
        }
        for f in &s.fields {
            if nutsh_catalog::ColumnKind::from_name(&f.kind).is_none() {
                problems.push(format!(
                    "{id}: unknown column kind {:?} for detail field {:?}",
                    f.kind, f.label
                ));
            }
            let width = f.label.chars().count();
            if width > nutsh_catalog::DETAIL_LABEL {
                problems.push(format!(
                    "{id}: detail label {:?} is {width} characters, at most {}",
                    f.label,
                    nutsh_catalog::DETAIL_LABEL
                ));
            }
        }
    }
    problems
}

/// Check 5: a `when` parses as one of the three forms, its path resolves, and - when the path's
/// last segment is `$objectType` - its literal is the short name of exactly one arm of the
/// property the prefix names.
///
/// The arm rule catches two things a path check cannot: a typo in the literal (`UefiBoott`
/// matches no arm) and an ambiguous one (two arms with the same short name). A `$objectType`
/// *presence* test is refused outright: every object a Prism Central emits carries one, so the
/// condition is always true and says nothing.
fn check_when(ns: &serde_json::Value, k: &KindModel, when: &str, what: &str) -> Option<String> {
    let Some(parsed) = nutsh_catalog::parse_when(when) else {
        return Some(format!(
            "{}: {what} when {when:?} is not `path`, `!path` or `path = VALUE`",
            k.id
        ));
    };
    let (path, literal) = match parsed {
        nutsh_catalog::When::Present(p) | nutsh_catalog::When::Absent(p) => (p, None),
        nutsh_catalog::When::Equals { path, value } => (path, Some(value)),
    };
    let Some(prefix) = nutsh_catalog::object_type_prefix(path) else {
        return (!super::parse::resolves(ns, &k.schema, path)).then(|| {
            format!(
                "{}: {what} when path {path:?} resolves against nothing in {}",
                k.id, k.schema
            )
        });
    };
    let Some(literal) = literal else {
        return Some(format!(
            "{}: {what} when {when:?} tests $objectType for presence or absence; every object \
             carries one, so the answer is fixed - write `{path} = VARIANT` instead",
            k.id
        ));
    };
    if !super::parse::resolves(ns, &k.schema, prefix) {
        return Some(format!(
            "{}: {what} when path prefix {prefix:?} resolves against nothing in {}",
            k.id, k.schema
        ));
    }
    // One entry per arm, repeats included: a `oneOf` whose arms share a short name is what makes
    // a literal ambiguous, and counting is how that is found. The list is de-duplicated only
    // where it is printed, since a repeat in a message is noise rather than news.
    let arms = super::parse::variant_names(ns, &k.schema, prefix);
    let matched = arms
        .iter()
        .filter(|a| a.eq_ignore_ascii_case(literal))
        .count();
    match matched {
        1 => None,
        0 => Some(format!(
            "{}: {what} when {when:?} names no arm of {prefix:?} (arms: {})",
            k.id,
            if arms.is_empty() {
                "none - the property is not polymorphic".to_string()
            } else {
                let mut distinct: Vec<&str> = Vec::new();
                for a in &arms {
                    if !distinct.contains(&a.as_str()) {
                        distinct.push(a);
                    }
                }
                distinct.join(", ")
            }
        )),
        n => Some(format!(
            "{}: {what} when {when:?} matches {n} arms of {prefix:?}, so it cannot say which; \
             the arms differ only in their version segments and the short name is all a wire tag \
             carries",
            k.id
        )),
    }
}
