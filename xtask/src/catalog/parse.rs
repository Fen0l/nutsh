//! Turn one OpenAPI document into `KindModel`s.

use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use nutsh_catalog::words::capitalize;
/// The acronym table lives in the catalog, so the generator's headers and the runtime's enum
/// values cannot drift apart. Re-exported because callers name it `parse::prettify`.
pub use nutsh_catalog::words::prettify;
use nutsh_catalog::{NAME_KEYS, RateLimit};
use serde_json::{Map, Value};

use super::model::{ActionModel, ColumnModel, FieldKind, FieldModel, KindModel, ListParamsModel};
use super::spec::SpecFile;

/// One namespace's kinds plus the `$actions` paths nothing claimed. An unattached action is a
/// missing feature, not drift, so it is reported and never fatal; `curated.toml` claims one
/// explicitly with `extra_actions`.
#[derive(Debug)]
pub struct Parsed {
    pub kinds: Vec<KindModel>,
    pub unattached: Vec<ActionModel>,
}

pub fn parse_namespace(spec: &SpecFile, doc: &Value) -> Result<Vec<KindModel>> {
    Ok(parse_namespace_full(spec, doc)?.kinds)
}

pub fn parse_namespace_full(spec: &SpecFile, doc: &Value) -> Result<Parsed> {
    let paths = doc
        .get("paths")
        .and_then(Value::as_object)
        .context("spec has no paths")?;
    let schemas = doc
        .pointer("/components/schemas")
        .cloned()
        .unwrap_or(Value::Object(Map::new()));

    let mut kinds = collect_list_kinds(spec, paths, &schemas);
    assign_ids(spec, &mut kinds);
    assign_parents(&mut kinds);
    // A list path that still carries a placeholder after parent resolution cannot be listed
    // on its own; mark it like a list that needs parameters so probes and recorders skip it.
    for k in kinds.iter_mut() {
        if k.parent.is_none() && k.list_path.contains('{') {
            k.list_params.required = true;
        }
    }
    for k in kinds.iter_mut() {
        k.actions = actions_for(&schemas, paths, &k.list_path, k.get_path.as_deref())?;
        clear_unfetchable_etags(k);
        k.rate = paths
            .get(&k.list_path)
            .and_then(|item| item.get("get").map(|op| rate_of(item, op, &k.list_path)))
            .transpose()?
            .unwrap_or(RateLimit::DEFAULT);
        k.fallback_columns = if k.schema.is_empty() {
            Vec::new()
        } else {
            fallback_columns(&schemas, &k.schema)
        };
        k.timestamps = if k.schema.is_empty() {
            Vec::new()
        } else {
            timestamp_props(&schemas, &k.schema)
        };
        k.properties = if k.schema.is_empty() {
            Vec::new()
        } else {
            schema_properties(&schemas, &k.schema)
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        };
        if k.fallback_columns.is_empty() {
            // A collection whose item schema the generator could not find: `microseg`'s policy
            // list declares `data: {}` and nothing more, and an array of strings names no
            // schema either. A dim eight-character stub beats a 36-character UUID while such a
            // kind waits for curation - which is what `microseg.config.policies` has.
            k.fallback_columns = vec![
                column("NAME", "name", "Text"),
                column("EXT ID", "extId", "Reference"),
            ];
        }
        k.display = derive_display(&k.id);
        k.aliases = derive_aliases(&k.list_path);
    }
    let unattached = attach_orphan_actions(&schemas, paths, &mut kinds)?;
    kinds.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Parsed { kinds, unattached })
}

fn collect_list_kinds(
    spec: &SpecFile,
    paths: &Map<String, Value>,
    schemas: &Value,
) -> Vec<KindModel> {
    let mut kinds = Vec::new();
    for (path, item) in paths {
        if path.ends_with('}') {
            continue;
        }
        let Some(get) = item.get("get") else { continue };
        // A singleton read is not a list, whatever its path looks like. See
        // `lists_a_collection`.
        if !lists_a_collection(schemas, get) {
            continue;
        }
        let schema = list_item_schema(schemas, get).unwrap_or_default();
        let get_path = paths
            .keys()
            .find(|p| {
                p.starts_with(&format!("{path}/{{"))
                    && p.ends_with('}')
                    && p.matches('{').count() == path.matches('{').count() + 1
                    && paths[*p].get("get").is_some()
            })
            .cloned();
        kinds.push(KindModel {
            id: String::new(),
            namespace: spec.namespace.clone(),
            version: spec.version.clone(),
            since: spec.version.clone(),
            display: String::new(),
            aliases: Vec::new(),
            category: capitalize(&spec.namespace),
            poll_secs: 30,
            list_path: path.clone(),
            get_path,
            schema,
            select: None,
            list_params: list_params(item, get),
            orderby: None,
            probe_by: None,
            timestamps: Vec::new(),
            properties: Vec::new(),
            max_rows: None,
            parent: None,
            actions: Vec::new(),
            action_kind: None,
            action_parents: Vec::new(),
            rate: RateLimit::DEFAULT,
            ext_id_key: "extId".to_string(),
            name_path: String::new(),
            warm: false,
            columns: Vec::new(),
            detail: Vec::new(),
            fallback_columns: Vec::new(),
            preview: spec.preview,
            curated: false,
            status_roles: Vec::new(),
        });
    }
    kinds
}

/// Ids come from the schema name with the version removed. Top-level kinds claim the plain
/// id; later kinds sharing a schema get `<id>~<last path segment>`, then `~2`, `~3`, ...
fn assign_ids(spec: &SpecFile, kinds: &mut [KindModel]) {
    kinds.sort_by(|a, b| {
        let ka = (
            a.list_path.matches('{').count(),
            a.list_path.len(),
            &a.list_path,
        );
        let kb = (
            b.list_path.matches('{').count(),
            b.list_path.len(),
            &b.list_path,
        );
        ka.cmp(&kb)
    });
    let mut taken: HashSet<String> = HashSet::new();
    for k in kinds.iter_mut() {
        let base = if k.schema.is_empty() {
            path_id(&spec.version, &k.list_path)
        } else {
            strip_version(&k.schema, &spec.version)
        };
        let mut id = if taken.contains(&base) {
            format!("{base}~{}", last_segment(&k.list_path))
        } else {
            base
        };
        let stem = id.clone();
        let mut n = 2;
        while taken.contains(&id) {
            id = format!("{stem}~{n}");
            n += 1;
        }
        taken.insert(id.clone());
        k.id = id;
    }
}

/// A sub-resource's parent is the kind whose `get_path` is the longest prefix of its `list_path`.
fn assign_parents(kinds: &mut [KindModel]) {
    let get_paths: Vec<(String, String)> = kinds
        .iter()
        .filter_map(|k| k.get_path.as_ref().map(|g| (normalize(g), k.id.clone())))
        .collect();
    for k in kinds.iter_mut() {
        if !k.list_path.contains('{') {
            continue;
        }
        let norm = normalize(&k.list_path);
        let mut best: Option<(usize, String)> = None;
        for (gp, id) in &get_paths {
            if norm.starts_with(&format!("{gp}/"))
                && best.as_ref().is_none_or(|(len, _)| gp.len() > *len)
            {
                best = Some((gp.len(), id.clone()));
            }
        }
        k.parent = best.map(|(_, id)| id);
    }
}

/// A GET's `200` JSON response schema, resolved through an `...ApiResponse` wrapper. Only the
/// `200` response describes the entity; `4XX`/`5XX` carry the error schema.
fn json_response<'a>(schemas: &'a Value, get: &'a Value) -> Option<&'a Value> {
    let schema = get.pointer("/responses/200/content/application~1json/schema")?;
    match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => schemas.get(r.rsplit('/').next()?),
        None => Some(schema),
    }
}

/// The `items` of a `data` that is a collection. Pre-release specs model `data` as
/// `oneOf: [array of entity, ..., ErrorResponse]`.
fn data_items(data: &Value) -> Option<&Value> {
    data.get("items").or_else(|| {
        data.get("oneOf")?
            .as_array()?
            .iter()
            .find_map(|b| b.get("items"))
    })
}

/// Whether a GET can answer with a collection, which is the one thing a list kind cannot
/// survive being wrong about.
///
/// `lifecycle`'s `getConfig` answers with `data` as a single object, and the kind the
/// generator built on it failed every poll with `cannot decode response, expected a list, got
/// object` - for ever, since nothing about a malformed body ever comes right, and each
/// failure re-negotiated the namespace besides. Thirty kinds were in that shape. The refusal
/// belongs here rather than in a hand-written exclusion list, which would leave the generator
/// free to add the thirty-first.
///
/// The question is not "did the item schema resolve": `microseg`'s policy list declares
/// `data: {}` and is a perfectly good collection, and an array of strings is one with no
/// schema to name. Only a `data` the spec positively describes as one entity is refused.
fn lists_a_collection(schemas: &Value, get: &Value) -> bool {
    // A `200` that declares no JSON is nothing a table can hold either: the `/file` paths
    // answer with `application/octet-stream` or `application/pdf`, and `iam`'s
    // `saml-sp-metadata` with `text/xml`.
    let Some(response) = json_response(schemas, get) else {
        return false;
    };
    // A response with no `data` at all says nothing about the payload, and the generator has
    // always guessed a list there.
    let Some(data) = response.pointer("/properties/data") else {
        return true;
    };
    !describes_one_entity(schemas, data)
}

/// A `data` the spec positively describes as a single entity: a `$ref` to a schema that is not
/// itself an array, an inline `type: object`, or a pre-release `oneOf` of those.
fn describes_one_entity(schemas: &Value, data: &Value) -> bool {
    let target = data
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| schemas.get(r.rsplit('/').next()?))
        .unwrap_or(data);
    let array = |v: &Value| {
        data_items(v).is_some() || v.get("type").and_then(Value::as_str) == Some("array")
    };
    if array(data) || array(target) {
        return false;
    }
    data.get("$ref").is_some()
        || target.get("type").and_then(Value::as_str) == Some("object")
        || data
            .get("oneOf")
            .and_then(Value::as_array)
            .is_some_and(|branches| branches.iter().any(|b| b.get("$ref").is_some()))
}

fn list_item_schema(schemas: &Value, get: &Value) -> Option<String> {
    let data = json_response(schemas, get)?.pointer("/properties/data")?;
    data_items(data)?
        .get("$ref")?
        .as_str()?
        .rsplit('/')
        .next()
        .map(str::to_string)
}

fn parameters<'a>(item: &'a Value, op: &'a Value) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for src in [item, op] {
        if let Some(ps) = src.get("parameters").and_then(Value::as_array) {
            out.extend(ps.iter());
        }
    }
    out
}

fn list_params(item: &Value, get: &Value) -> ListParamsModel {
    let params = parameters(item, get);
    let has = |name: &str| {
        params
            .iter()
            .any(|p| p.get("name").and_then(Value::as_str) == Some(name))
    };
    let required = params.iter().any(|p| {
        p.get("in").and_then(Value::as_str) == Some("query")
            && p.get("required").and_then(Value::as_bool) == Some(true)
    });
    ListParamsModel {
        page: has("$page"),
        limit: has("$limit"),
        filter: has("$filter"),
        orderby: has("$orderby"),
        select: has("$select"),
        expand: has("$expand"),
        required,
        orderby_fields: odata_fields(&params, "$orderby"),
    }
}

/// The `x-odata-fields` allowlist a query parameter declares, in order, or empty when it
/// declares none. Today `list_params` is a presence check and throws this away, which is why
/// nobody has been bitten by it and why nobody has benefited from it either.
fn odata_fields(params: &[&Value], name: &str) -> Vec<String> {
    params
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|p| p.get("x-odata-fields"))
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| f.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `vmm.v4.3.ahv.config.Vm` → `vmm.ahv.config.Vm`; `storage.v4.r0.a3.config.VolumeGroup` → `storage.config.VolumeGroup`.
fn strip_version(schema: &str, version: &str) -> String {
    let marker = format!(".{version}.");
    if schema.contains(&marker) {
        return schema.replacen(&marker, ".", 1);
    }
    strip_version_segments(schema)
}

/// A schema name with every version segment removed: `microseg.v4.3.config.NetworkSecurityPolicy`
/// → `microseg.config.NetworkSecurityPolicy`. What a curated `schema` override is written as, so
/// a spec bump does not rot `curated.toml`.
pub fn strip_version_segments(schema: &str) -> String {
    schema
        .split('.')
        .filter(|seg| !is_version_segment(seg))
        .collect::<Vec<_>>()
        .join(".")
}

/// `v4`, `r0`, `a3`, `b1`, or a bare number.
fn is_version_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    if seg.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let mut chars = seg.chars();
    matches!(chars.next(), Some('v' | 'r' | 'a' | 'b'))
        && seg.len() > 1
        && chars.all(|c| c.is_ascii_digit())
}

/// `/storage/v4.0.a3/config/storage-containers` → `storage.config.storage-containers`.
fn path_id(version: &str, path: &str) -> String {
    path.trim_start_matches('/')
        .split('/')
        .filter(|s| *s != version && !s.starts_with('{'))
        .collect::<Vec<_>>()
        .join(".")
}

/// Replace every `{name}` with `{}` so paths with different placeholder names compare equal.
pub(crate) fn normalize(path: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in path.chars() {
        match c {
            '{' => {
                inside = true;
                out.push('{');
            }
            '}' => {
                inside = false;
                out.push('}');
            }
            _ if inside => {}
            _ => out.push(c),
        }
    }
    out
}

/// The last two segments of a path, placeholder names normalised: `hosts/{}` for both
/// `/config/clusters/{clusterExtId}/hosts/{extId}` and
/// `/operations/clusters/{clusterExtId}/hosts/{extId}`.
fn trailing_two(path: &str) -> Option<String> {
    let n = normalize(path);
    let mut segments = n.rsplit('/');
    let last = segments.next()?;
    let before = segments.next()?;
    Some(format!("{before}/{last}"))
}

fn last_segment(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// `Vm` → `VMs`; `storage-containers` → `Storage Containers`; `AwsSubnet` → `Aws Subnets`.
fn derive_display(id: &str) -> String {
    let last = id.rsplit('.').next().unwrap_or(id);
    let last = last.split('~').next().unwrap_or(last);
    if last.contains('-') || last.chars().all(|c| c.is_ascii_lowercase()) {
        return prettify(
            &last
                .split('-')
                .map(capitalize)
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    let mut words = String::new();
    for (i, c) in last.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            words.push(' ');
        }
        words.push(c);
    }
    prettify(&pluralize(&words))
}

/// `Policy` → `Policies`, `Address` → `Addresses`, `Stats` → `Stats`, `Vm` → `Vms`.
pub fn pluralize(word: &str) -> String {
    if let Some(stem) = word.strip_suffix('y')
        && !stem.ends_with(|c: char| "aeiouAEIOU".contains(c))
    {
        return format!("{stem}ies");
    }
    if word.ends_with("ss") || word.ends_with('x') || word.ends_with("ch") || word.ends_with("sh") {
        return format!("{word}es");
    }
    if word.ends_with('s') {
        return word.to_string();
    }
    format!("{word}s")
}

/// `vms` → `vm`, `policies` → `policy`, `addresses` → `address`; `status`/`stats` → none.
pub fn singular(seg: &str) -> Option<String> {
    if let Some(stem) = seg.strip_suffix("ies") {
        return Some(format!("{stem}y"));
    }
    if seg.ends_with("sses")
        || seg.ends_with("xes")
        || seg.ends_with("ches")
        || seg.ends_with("shes")
    {
        return seg.strip_suffix("es").map(str::to_string);
    }
    if seg.ends_with("ss") || seg.ends_with("us") || seg.ends_with("is") || seg.ends_with("ats") {
        return None;
    }
    seg.strip_suffix('s')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Last path segment plus its singular: `/config/vms` → `["vms", "vm"]`.
fn derive_aliases(list_path: &str) -> Vec<String> {
    let seg = last_segment(list_path);
    let mut out = vec![seg.clone()];
    if let Some(s) = singular(&seg) {
        out.push(s);
    }
    out
}

pub fn column(header: &str, path: &str, kind: &str) -> ColumnModel {
    ColumnModel {
        header: header.to_string(),
        path: path.to_string(),
        kind: kind.to_string(),
    }
}

/// `create` (POST list), `update`/`patch`/`delete` (on the get path), and every
/// `<get_path>/$actions/<name>` POST. Sorted by name.
fn actions_for(
    schemas: &Value,
    paths: &Map<String, Value>,
    list_path: &str,
    get_path: Option<&str>,
) -> Result<Vec<ActionModel>> {
    let mut out = Vec::new();
    if let Some(item) = paths.get(list_path)
        && let Some(op) = item.get("post")
    {
        out.push(action(schemas, "create", list_path, "Post", item, op)?);
    }
    if let Some(gp) = get_path {
        if let Some(item) = paths.get(gp) {
            for (name, verb, key) in [
                ("update", "Put", "put"),
                ("patch", "Patch", "patch"),
                ("delete", "Delete", "delete"),
            ] {
                if let Some(op) = item.get(key) {
                    out.push(action(schemas, name, gp, verb, item, op)?);
                }
            }
        }
        // Action paths often rename the placeholder (`{extId}` vs `{clusterExtId}`); compare shapes.
        let prefix = format!("{}/$actions/", normalize(gp));
        for (p, item) in paths {
            if let Some(rest) = normalize(p).strip_prefix(&prefix)
                && !rest.contains('/')
                && let Some(op) = item.get("post")
            {
                out.push(action(schemas, rest, p, "Post", item, op)?);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn action(
    schemas: &Value,
    name: &str,
    path: &str,
    method: &str,
    item: &Value,
    op: &Value,
) -> Result<ActionModel> {
    let declares_if_match = op
        .get("parameters")
        .and_then(Value::as_array)
        .is_some_and(|ps| {
            ps.iter()
                .any(|p| p.get("name").and_then(Value::as_str) == Some("If-Match"))
        });
    // A create has no entity yet, so it can never carry an ETag even if the spec lists If-Match.
    // Entity actions need one even where the spec omits it (legacy lessons 3 to 5); §5.4's two
    // exceptions are overridden in `curated.toml`, not here.
    let entity_action = method == "Post" && path.contains("/$actions/") && path.contains('{');
    let needs_etag = matches!(method, "Put" | "Patch" | "Delete")
        || entity_action
        || (declares_if_match && method != "Post");
    // Two questions, not one. `takes_body`: the operation declares a `requestBody` at all.
    // `needs_body`: it declares that body `required: true`. `$actions/clone` declares one
    // without `required`, so it is optional by OpenAPI default, while `$actions/guest-shutdown`
    // declares one that is required. The mock answers the way a Prism Central does: it rejects
    // a body where `!takes_body` - a body on a VM power action is an HTTP 400 "no body
    // expected" - and demands one where `needs_body`.
    let takes_body = op.get("requestBody").is_some();
    let needs_body = op.pointer("/requestBody/required").and_then(Value::as_bool) == Some(true);
    let roles = op
        .pointer("/x-permissions/roleList")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .filter(|r| r.get("deprecated").and_then(Value::as_bool) != Some(true))
                .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(ActionModel {
        name: name.to_string(),
        path: path.to_string(),
        method: method.to_string(),
        needs_etag,
        needs_body,
        takes_body,
        returns: returns_of(op),
        scope: scope_of(path),
        roles,
        key: String::new(),
        label: String::new(),
        danger: "None".to_string(),
        confirm: "None".to_string(),
        order: 0,
        hidden: false,
        body: None,
        form: form_for(schemas, op),
        rate: rate_of(item, op, path)?,
    })
}

/// Fields from the **top-level** properties of an action's request schema. A nested object or
/// array becomes one `Json` field seeded from its own required properties - the escape hatch a
/// power user needs, and no worse than a text editor.
fn form_for(schemas: &Value, op: &Value) -> Vec<FieldModel> {
    let Some(schema) = op.pointer("/requestBody/content/application~1json/schema") else {
        return Vec::new();
    };
    let required = required_of(resolve(schemas, schema));
    let mut props = Vec::new();
    properties(schemas, schema, 0, &mut props);
    props
        .iter()
        .filter(|(name, prop)| !name.starts_with('$') && !is_read_only(schemas, prop))
        .map(|(name, prop)| FieldModel {
            name: name.clone(),
            label: label_for(name),
            ty: field_kind(schemas, prop),
            required: required.contains(&name.as_str()),
            value: None,
            hidden: false,
        })
        .collect()
}

/// A `readOnly` property is the server's to fill: `extId`, `links`, `tenantId` and the
/// timestamps reach every create and update body through the common base model, and a body
/// that carries one is what Prism rejects. `SKIP_PROPS` is the columns' list and is not
/// reused here: it drops `description`, which a form must keep.
fn is_read_only(schemas: &Value, prop: &Value) -> bool {
    let flag = |v: &Value| v.get("readOnly").and_then(Value::as_bool) == Some(true);
    flag(prop) || flag(resolve(schemas, prop))
}

/// `powerState` → `Power state`; the sentence case a form label wants, against the
/// SCREAMING header a column wants.
fn label_for(prop: &str) -> String {
    let header = header_for(prop);
    let mut chars = header.chars();
    match chars.next() {
        Some(first) => first.to_string() + &chars.as_str().to_ascii_lowercase(),
        None => String::new(),
    }
}

fn field_kind(schemas: &Value, prop: &Value) -> FieldKind {
    let short = prop
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.rsplit('/').next())
        .map(str::to_string);
    let resolved = resolve(schemas, prop);
    match schema_type(resolved) {
        "boolean" => FieldKind::Bool,
        "integer" | "number" => FieldKind::Integer,
        "string" => match resolved.get("enum").and_then(Value::as_array) {
            Some(vs) => FieldKind::Enum(
                vs.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            ),
            None => FieldKind::Text,
        },
        "array" => {
            // One seeded item for an array of objects; an array of scalars (`categoryExtIds`,
            // `enabledCpuFeatures`) starts empty, since `[{}]` is a wrong body for it.
            let items = resolved.get("items").unwrap_or(&Value::Null);
            if schema_type(resolve(schemas, items)) == "object" {
                FieldKind::Json(format!("[{}]", seed(schemas, items)))
            } else {
                FieldKind::Json("[]".to_string())
            }
        }
        "object" => {
            // A `{extId}` reference is a picker, not a JSON blob; the short name is resolved
            // to a kind id once every namespace is parsed.
            let mut sub = Vec::new();
            properties(schemas, resolved, 0, &mut sub);
            if sub.iter().any(|(n, _)| n == "extId" || n == "uuid") {
                FieldKind::Reference(short)
            } else {
                FieldKind::Json(seed(schemas, prop))
            }
        }
        _ => FieldKind::Text,
    }
}

/// The names a resolved schema's `required` list declares; empty when it declares none.
fn required_of(resolved: &Value) -> Vec<&str> {
    resolved
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// A JSON skeleton for an object: its required properties with null values, `{}` when it
/// declares none. Written by hand so the text is byte-identical on every run.
fn seed(schemas: &Value, schema: &Value) -> String {
    let required = required_of(resolve(schemas, schema));
    if required.is_empty() {
        return "{}".to_string();
    }
    let body = required
        .iter()
        .map(|k| format!("{}: null", serde_json::to_string(k).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

/// Turn every `FieldKind::Reference(<schema short name>)` into a kind id. Runs once, over
/// every namespace, because a reference may name a kind another namespace owns.
pub fn resolve_field_references(kinds: &mut [KindModel]) {
    // (schema's last segment, whole schema, kind id) per kind; owned, since `kinds` is
    // rewritten below.
    let by_last: Vec<(String, String, String)> = kinds
        .iter()
        .filter_map(|k| {
            let last = k.schema.rsplit('.').next()?;
            Some((last.to_string(), k.schema.clone(), k.id.clone()))
        })
        .collect();
    let resolve_one = |short: &str| -> Option<String> {
        let last = short.rsplit('.').next().unwrap_or(short);
        let bare = last.strip_suffix("Reference").unwrap_or(last);
        let hits: Vec<(&str, &str)> = by_last
            .iter()
            .filter(|(l, _, _)| l == bare)
            .map(|(_, schema, id)| (schema.as_str(), id.as_str()))
            .collect();
        // Exactly one schema, or nothing: an ambiguous name is a plain extId text box, which
        // is honest, where a guess would open a picker over the wrong kind. Several kinds
        // over one schema are one listing and its nested `~` views (`Host`, `Host~hosts`),
        // and the top-level one, whose id carries no `~`, is the picker's.
        let (first, _) = *hits.first()?;
        if hits.iter().any(|(schema, _)| *schema != first) {
            return None;
        }
        hits.iter()
            .find(|(_, id)| !id.contains('~'))
            .map(|(_, id)| (*id).to_string())
    };
    for k in kinds.iter_mut() {
        for a in k.actions.iter_mut() {
            for f in a.form.iter_mut() {
                if let FieldKind::Reference(Some(short)) = &f.ty {
                    f.ty = FieldKind::Reference(resolve_one(short));
                }
            }
        }
    }
}

/// Clears `needs_etag` on every action whose ETag could never be fetched. An ETag comes from
/// `get_in`, which fills exactly the get path's placeholders, so an action whose path carries
/// a different number of them - or a kind with no get path at all - has no way to obtain one.
/// The flag is cleared with a note rather than left as a trap for the client. Every action
/// `actions_for` attaches sits on the get path itself, so only an `/operations/…` path
/// attached by `attach_orphan_actions` or claimed through `extra_actions` can diverge; both
/// run this again after attaching.
pub(crate) fn clear_unfetchable_etags(k: &mut KindModel) {
    let get_count = k.get_path.as_deref().map(nutsh_catalog::placeholder_count);
    for a in k.actions.iter_mut() {
        let fetchable = get_count == Some(nutsh_catalog::placeholder_count(&a.path));
        if a.needs_etag && !fetchable {
            eprintln!(
                "  note: {} {} wants an ETag its get path cannot fetch; cleared",
                k.id, a.name
            );
            a.needs_etag = false;
        }
    }
}

/// Claim every `$actions` POST the first pass left. The prefix's trailing two segments name the
/// kind: exactly one kind of this namespace whose `get_path` ends in the same two attaches it;
/// zero or several leave it unattached.
fn attach_orphan_actions(
    schemas: &Value,
    paths: &Map<String, Value>,
    kinds: &mut [KindModel],
) -> Result<Vec<ActionModel>> {
    let claimed: HashSet<String> = kinds
        .iter()
        .flat_map(|k| k.actions.iter().map(|a| a.path.clone()))
        .collect();
    let mut orphans = Vec::new();
    for (path, item) in paths {
        let Some((prefix, name)) = path.split_once("/$actions/") else {
            continue;
        };
        if name.contains('/') || claimed.contains(path) {
            continue;
        }
        let Some(op) = item.get("post") else {
            continue;
        };
        let model = action(schemas, name, path, "Post", item, op)?;
        let Some(tail) = trailing_two(prefix) else {
            orphans.push(model);
            continue;
        };
        let owners: Vec<usize> = kinds
            .iter()
            .enumerate()
            .filter(|(_, k)| {
                k.get_path
                    .as_deref()
                    .and_then(trailing_two)
                    .is_some_and(|t| t == tail)
            })
            .map(|(i, _)| i)
            .collect();
        match owners.as_slice() {
            [only] => {
                // The prefix must also carry the same ids the get path does, or the ETag fetch
                // could not fill them; the same clearing the first pass runs settles it.
                let owner = &mut kinds[*only];
                owner.actions.push(model);
                clear_unfetchable_etags(owner);
                owner.actions.sort_by(|a, b| a.name.cmp(&b.name));
            }
            _ => orphans.push(model),
        }
    }
    orphans.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(orphans)
}

/// Rate tiers the specs use. `default` is not a size class, so it folds onto the unnamed tier
/// along with an entry that declares no `type` at all; both are observed in `specs/`.
const RATE_TIERS: &[&str] = &["xsmall", "small", "large", "xlarge", "default"];

/// The tightest `x-rate-limit` tier of an operation, or the default budget when it declares
/// none. Casing is inconsistent across the specs (`xsmall` beside `Small`), so the type is
/// lower-cased; a tier outside `RATE_TIERS` fails generation so a new one is noticed rather
/// than dropped.
fn rate_of(item: &Value, op: &Value, path: &str) -> Result<RateLimit> {
    // One stray path-item-level declaration exists (`specs/files/v4.0.yaml`); an operation's
    // own list wins where both are present.
    let entries = op
        .get("x-rate-limit")
        .or_else(|| item.get("x-rate-limit"))
        .and_then(Value::as_array);
    let Some(entries) = entries else {
        return Ok(RateLimit::DEFAULT);
    };
    let mut best: Option<RateLimit> = None;
    for e in entries {
        let tier = e
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_ascii_lowercase();
        if !RATE_TIERS.contains(&tier.as_str()) {
            bail!("{path}: unknown x-rate-limit tier {tier:?}; known: {RATE_TIERS:?}");
        }
        // Every entry in `specs/` spells its unit; a missing one is refused like an unknown
        // one rather than read as `seconds`, the loosest interpretation.
        let per_secs = match e.get("timeUnit").and_then(Value::as_str) {
            Some("seconds") => 1,
            Some("minutes") => 60,
            other => bail!("{path}: unknown x-rate-limit timeUnit {other:?}"),
        };
        // A zero or missing count would be a budget of nothing; treat it as one request.
        let count = match e.get("count").and_then(Value::as_u64) {
            Some(c) => u32::try_from(c).unwrap_or(u32::MAX).max(1),
            None => 1,
        };
        let candidate = RateLimit { count, per_secs };
        if best.is_none_or(|b| candidate.tighter_than(b)) {
            best = Some(candidate);
        }
    }
    Ok(best.unwrap_or(RateLimit::DEFAULT))
}

/// What a 2xx answer carries. The 202 decides; without one, the same walk over 200 and 201,
/// which is how task `cancel` comes out `Payload` - its 200 carries an `AppMessage`, not a
/// task. A `oneOf` with a `TaskReference` arm is accepted defensively: no 2xx `data` in
/// `specs/` is one today, the arm costs three lines, and it saves a silent misclassification
/// when one appears.
fn returns_of(op: &Value) -> String {
    for code in ["202", "200", "201"] {
        let Some(resp) = op.pointer(&format!("/responses/{code}")) else {
            continue;
        };
        let Some(schema) = resp.pointer("/content/application~1json/schema") else {
            return "None".to_string();
        };
        let Some(data) = schema.pointer("/properties/data") else {
            return "Payload".to_string();
        };
        return if refers_to_task(data) {
            "Task".to_string()
        } else {
            "Payload".to_string()
        };
    }
    "None".to_string()
}

fn refers_to_task(data: &Value) -> bool {
    let direct = data.get("$ref").and_then(Value::as_str);
    let arms = data
        .get("oneOf")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|b| b.get("$ref").and_then(Value::as_str))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    direct
        .into_iter()
        .chain(arms)
        .any(|r| r.ends_with("TaskReference"))
}

/// Placeholder names of `path`, left to right.
fn scope_of(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        out.push(rest[start + 1..start + end].to_string());
        rest = &rest[start + end + 1..];
    }
    out
}

const SKIP_PROPS: &[&str] = &[
    "extId",
    "links",
    "tenantId",
    "description",
    // 31 nav kinds carry it, `listProjects` exists only in the beta
    // `specs/multidomain/v4.4.b1.yaml` (and the catalog pins `multidomain` at v4.3), and it was
    // empty on every lab row inspected. A column that can never resolve and is always empty is
    // not a column.
    "projectExtId",
];
const MAX_FALLBACK_COLUMNS: usize = 20;

fn resolve<'a>(schemas: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => schemas
            .get(r.rsplit('/').next().unwrap_or(""))
            .unwrap_or(schema),
        None => schema,
    }
}

/// A resolved schema's `type`, taking one that declares `properties` or `allOf` without a
/// `type` as an object; `""` when nothing says.
fn schema_type(resolved: &Value) -> &str {
    resolved.get("type").and_then(Value::as_str).unwrap_or(
        if resolved.get("properties").is_some() || resolved.get("allOf").is_some() {
            "object"
        } else {
            ""
        },
    )
}

/// Top-level properties of a schema, following `$ref` and `allOf` (depth-limited), in order.
fn properties(schemas: &Value, schema: &Value, depth: u8, out: &mut Vec<(String, Value)>) {
    if depth > 4 {
        return;
    }
    let schema = resolve(schemas, schema);
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        for s in all {
            properties(schemas, s, depth + 1, out);
        }
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (k, v) in props {
            if !out.iter().any(|(n, _)| n == k) {
                out.push((k.clone(), v.clone()));
            }
        }
    }
}

/// Whether `path` resolves against `schema_name`. [`property_at`] describes the walk; this is
/// that walk asked whether it arrived.
pub fn resolves(schemas: &Value, schema_name: &str, path: &str) -> bool {
    property_at_named(schemas, schema_name, path).is_some()
}

/// The schema of the property at `path` in `schema_name`, **unresolved**; `None` when the path
/// resolves against nothing.
///
/// [`resolves`] is this and a `.is_some()`, and [`variant_names`] is this and a look at the arm
/// list: one walk, so "does this path exist" and "what can its `$objectType` say" cannot drift
/// apart. It was two copies of the same twenty lines until they nearly did.
fn property_at_named(schemas: &Value, schema_name: &str, path: &str) -> Option<Value> {
    let schema = schemas.get(schema_name)?;
    property_at(schemas, schema, path, 0)
}

/// A schema and, when it is polymorphic, every `oneOf`/`anyOf` arm of it.
fn arms(schemas: &Value, schema: &Value) -> Vec<Value> {
    let resolved = resolve(schemas, schema);
    let mut out = vec![resolved.clone()];
    for key in ["oneOf", "anyOf"] {
        if let Some(list) = resolved.get(key).and_then(Value::as_array) {
            out.extend(list.iter().map(|a| resolve(schemas, a).clone()));
        }
    }
    out
}

/// The short names of the `oneOf`/`anyOf` arms of the property at `path` in `schema_name`, in
/// declaration order: the variant tags a `$objectType` at that path can carry.
///
/// Names, not schemas: [`arms`] resolves each `$ref` into the value it points at, and the value
/// does not carry the name a `when` compares against. The comparison is on the last segment of
/// the schema key, because the wire tag and the schema key spell the version differently
/// (`vmm.v4.ahv.config.VmDisk` against `vmm.v4.r0.b1.ahv.config.VmDisk` - both are in the
/// committed fixtures, on the same shape) and the short name is the only half that is stable.
///
/// **One entry per arm, duplicates included.** A `oneOf` whose arms share a short name is exactly
/// what makes a `when` ambiguous, and it is a real shape in this project's data: the two
/// spellings above differ only in their version segments, so a schema that listed both would
/// give two arms named `VmDisk` and no way to tell which a tag meant. De-duplicating here would
/// hide that from `check_when`, which counts the matches; the caller de-duplicates when it
/// prints the list, which is the only place a repeat is merely noise.
pub fn variant_names(schemas: &Value, schema_name: &str, path: &str) -> Vec<String> {
    let Some(prop) = property_at_named(schemas, schema_name, path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for key in ["oneOf", "anyOf"] {
        let Some(list) = prop.get(key).and_then(Value::as_array) else {
            continue;
        };
        for arm in list {
            let Some(r) = arm.get("$ref").and_then(Value::as_str) else {
                continue;
            };
            let name = r.rsplit('/').next().unwrap_or(r);
            out.push(name.rsplit('.').next().unwrap_or(name).to_string());
        }
    }
    out
}

/// The schema of the property at `path`, **unresolved**: the walk stops one step short of
/// `resolve` on the last segment, because an arm list lives on the property as the document
/// writes it and `resolve` would replace a `$ref` with what it names and lose the names.
///
/// Descends through `[]` at an array property into its items and through every `oneOf`/`anyOf`
/// arm at a polymorphic one, the way [`fallback_columns`] does; `allOf` needs no arm, because
/// [`properties`] already follows it. A path that runs out of segments lands on whatever it
/// reached, because the last segment of a column path is a scalar and this walk is about the
/// segments before it.
fn property_at(schemas: &Value, schema: &Value, path: &str, depth: u8) -> Option<Value> {
    if depth > 8 {
        return None;
    }
    let Some((segment, rest)) = split_first(path) else {
        return Some(schema.clone());
    };
    let (key, fan_out) = match segment.strip_suffix("[]") {
        Some(k) => (k, true),
        None => (segment, false),
    };
    for arm in arms(schemas, schema) {
        let mut props = Vec::new();
        properties(schemas, &arm, 0, &mut props);
        let Some((_, prop)) = props.iter().find(|(n, _)| n == key) else {
            continue;
        };
        let next = if fan_out {
            match resolve(schemas, prop).get("items") {
                Some(items) => items.clone(),
                None => continue,
            }
        } else {
            prop.clone()
        };
        if let Some(found) = property_at(schemas, &next, rest, depth + 1) {
            return Some(found);
        }
    }
    None
}

fn split_first(path: &str) -> Option<(&str, &str)> {
    if path.is_empty() {
        return None;
    }
    Some(match path.split_once('.') {
        Some((head, rest)) => (head, rest),
        None => (path, ""),
    })
}

/// Every top-level property of `schema_name`, following `$ref` and `allOf`, in the order the
/// document declares them. The generator's own view of a schema, exposed so the overlay checks
/// ask exactly the question `fallback_columns` asks and cannot drift from it.
pub fn schema_properties(schemas: &Value, schema_name: &str) -> Vec<(String, Value)> {
    let Some(schema) = schemas.get(schema_name) else {
        return Vec::new();
    };
    let mut props = Vec::new();
    properties(schemas, schema, 0, &mut props);
    props
}

/// Top-level properties the schema declares as an RFC 3339 timestamp: the set a curated
/// `probe_by` must come from. The same question [`fallback_columns`] asks of one property,
/// asked of every one of them, so a probe field and a `Timestamp` column cannot disagree.
fn timestamp_props(schemas: &Value, schema_name: &str) -> Vec<String> {
    schema_properties(schemas, schema_name)
        .into_iter()
        .filter(|(_, prop)| {
            let prop = resolve(schemas, prop);
            schema_type(prop) == "string"
                && prop.get("format").and_then(Value::as_str) == Some("date-time")
        })
        .map(|(name, _)| name)
        .collect()
}

/// Name first, then enums, timestamps, references, other scalars, and array counts.
/// References precede scalars so the 20-column cap never drops `cluster`/`host` on big kinds.
pub fn fallback_columns(schemas: &Value, schema_name: &str) -> Vec<ColumnModel> {
    let props = schema_properties(schemas, schema_name);

    let (mut names, mut enums, mut times, mut scalars, mut refs, mut counts) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for (name, prop) in &props {
        if name.starts_with('$') || SKIP_PROPS.contains(&name.as_str()) {
            continue;
        }
        let prop = resolve(schemas, prop);
        let ty = schema_type(prop);
        if is_secret_property(name, prop, ty) {
            continue;
        }
        let header = header_for(name);
        match ty {
            "string" if NAME_KEYS.contains(&name.as_str()) => {
                names.push(column(&header, name, "Text"))
            }
            "string" if prop.get("enum").is_some() => enums.push(column(&header, name, "Enum")),
            "string" if prop.get("format").and_then(Value::as_str) == Some("date-time") => {
                times.push(column(&header, name, "Timestamp"))
            }
            // Schema-driven, not a name list: one literal UUID pattern is written on every
            // identifier property in every spec, so this catches `createdBy`, `ownerUuid`,
            // `vpcReference` and `projectExtId` alike with nothing to keep up to date.
            "string" if is_uuid_pattern(prop) => refs.push(column(&header, name, "Reference")),
            "string" if is_ip_name(name) => scalars.push(column(&header, name, "Ip")),
            // A timestamp the spec forgot to mark. `cell::render` falls back to the raw text
            // when the value does not parse as RFC 3339, so the worst case is what it does now.
            "string" if is_time_name(name) => times.push(column(&header, name, "Timestamp")),
            "string" => scalars.push(column(&header, name, "Text")),
            "integer" | "number" if name.ends_with("Bytes") => {
                scalars.push(column(&header, name, "Bytes"))
            }
            "integer" | "number" if is_micros_name(name) => {
                scalars.push(column(&header, name, "Micros"))
            }
            "integer" | "number" if is_percent_name(name) => {
                scalars.push(column(&header, name, "Percent"))
            }
            "integer" | "number" if is_duration_name(name) => {
                scalars.push(column(&header, name, "Duration"))
            }
            "integer" | "number" => scalars.push(column(&header, name, "Text")),
            "boolean" => scalars.push(column(&header, name, "Bool")),
            "array" => counts.push(column(&header, name, "Count")),
            "object" => {
                let mut sub = Vec::new();
                properties(schemas, prop, 0, &mut sub);
                // Reference objects carry `extId`, or `uuid` on a few older shapes (Host.cluster).
                if let Some(key) = ["extId", "uuid"]
                    .iter()
                    .find(|k| sub.iter().any(|(n, _)| n == *k))
                {
                    refs.push(column(&header, &format!("{name}.{key}"), "Reference"));
                }
            }
            _ => {}
        }
    }
    dedup_references(&mut refs);
    let mut out = names;
    out.extend(enums);
    out.extend(times);
    out.extend(refs);
    out.extend(scalars);
    out.extend(counts);
    // Promote first, truncate second: an enum that would have carried the row tint but landed
    // at position 21 was otherwise never promoted.
    promote_status(&mut out);
    out.truncate(MAX_FALLBACK_COLUMNS);
    out
}

/// The one literal pattern Nutanix writes on every identifier property, verified on
/// `microseg.v4.3.config.ServiceGroup.createdBy`,
/// `prism.v4.4.config.DomainManager.hostingClusterExtId` and hundreds more.
fn is_uuid_pattern(prop: &Value) -> bool {
    prop.get("pattern")
        .and_then(Value::as_str)
        .is_some_and(|p| p.contains("[a-fA-F0-9]{8}-") && p.contains("[a-fA-F0-9]{12}"))
}

/// A property that must never be a column: the spec says it is write-only, or its name says it
/// is a credential.
///
/// The name half is [`nutsh_catalog::secret::is_secret_name`], the same rule the recorder
/// redacts a fixture with, so that what the leak scan takes out of a recording is what the
/// catalog refuses to show. It is applied to `string` properties only, which leaves the
/// legitimate `Bool` columns beside a secret - `isForceResetPasswordEnabled`, `hasPrivateKey`,
/// `shouldValidateAdCredential` - alone while still dropping `privateKey`,
/// `privateKeyPassphrase`, `password`, `targetSecret`, `clientSecret`, `secretAccessKey` and
/// `reclaimToken`, which is what the catalog shipped as ordinary columns on thirteen kinds.
fn is_secret_property(name: &str, prop: &Value, ty: &str) -> bool {
    if prop.get("writeOnly").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    ty == "string" && nutsh_catalog::secret::is_secret_name(name)
}

/// A string whose last segment ends `Time`, `Timestamp` or `Date` and that matched no earlier
/// arm.
fn is_time_name(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    last.ends_with("Time") || last.ends_with("Timestamp") || last.ends_with("Date")
}

/// A reference column's identity, for the de-duplication: drop a trailing `.extId`/`.uuid`,
/// then drop a trailing `Reference`/`ExtId`/`Uuid` from the last segment, lowercase.
pub fn reference_key(path: &str) -> String {
    let base = path
        .strip_suffix(".extId")
        .or_else(|| path.strip_suffix(".uuid"))
        .unwrap_or(path);
    let last = base.rsplit('.').next().unwrap_or(base);
    let trimmed = last
        .strip_suffix("Reference")
        .or_else(|| last.strip_suffix("ExtId"))
        .or_else(|| last.strip_suffix("Uuid"))
        .unwrap_or(last);
    trimmed.to_ascii_lowercase()
}

/// One column per reference base. The dotted form wins - `vpc.extId` is the expanded object and
/// `vpcReference` the legacy string beside it - and otherwise the first declared.
///
/// **Measured on two Prism Centrals, and kept.** The 2026-09-10 lab recording first suggested
/// the preference was backwards: four pairs - `networking.config.FloatingIp.vpc`, `.externalSubnet`,
/// `networking.config.Gateway.vpc` and `vmm.ahv.config.VmRecoveryPoint.vm` - fill the flat
/// sibling on every row and the dotted object on none, and a table sends no `$expand`, since
/// `ListOptions` has no such field. Both `fixtures-lab` and `fixtures-lab-sierra` agree on every
/// one of those pairs, so the second Prism Central did not overturn them.
///
/// What overturns the *rule* is the rest of the corpus. Over the twenty-nine dotted `.extId`
/// fallback columns whose kind either lab recorded, the object half is the one that fills on
/// fifteen: `sourceEntity` and `clusterReference` on alerts, audits and events; `parentTask`,
/// `rootTask` and `ownedBy` on tasks; `cluster`, `host` and `source` on VMs; `createdBy` and
/// `updatedBy` on templates and anti-affinity policies. Those are not expansions - they arrive
/// as embedded reference objects (`{"$objectType": "vmm.v4.ahv.config.ClusterReference",
/// "extId": …}`) on a plain list, and the flat `clusterReference`/`hostReference`/`ownedByReference`
/// siblings the schema declares beside them are never sent at all. Seven pairs go the other way,
/// six fill neither half, and one - `vmm.ahv.config.Vm.project` - fills both.
///
/// So the split is per-schema, not general: `networking` (and the recovery point) put the uuid
/// in the flat sibling, while `monitoring`, `prism` and `vmm` put it in the object. Flipping the
/// preference would break fifteen pairs to repair seven. Neither preference is right often
/// enough to be a rule, which is why the four kinds that need the other half are curated one by
/// one in `crates/catalog/curated.toml` and this stays as it is.
fn dedup_references(refs: &mut Vec<ColumnModel>) {
    let mut kept: Vec<ColumnModel> = Vec::new();
    for c in refs.drain(..) {
        let key = reference_key(&c.path);
        match kept.iter().position(|k| reference_key(&k.path) == key) {
            None => kept.push(c),
            Some(i) if !kept[i].path.contains('.') && c.path.contains('.') => kept[i] = c,
            Some(_) => {}
        }
    }
    *refs = kept;
}

/// `…Seconds` and `…Secs`: an integer of seconds is a duration, not a count. The mirror of the
/// `…Bytes` rule above it.
fn is_duration_name(name: &str) -> bool {
    name.ends_with("Seconds") || name.ends_with("Secs")
}

/// An address column: a property whose **last word** is `IP`.
///
/// It takes a property name (a bare top-level key, which is all [`fallback_columns`] ever has)
/// and asks [`header_for`] to split it, because that is where the camel-case word boundary
/// is already implemented and tested (plural runs and acronym runs included). The last word,
/// not a case-insensitive suffix: `ownership` and `membership` end in a lowercase `ip` that
/// continues a word, and a column of names capped at fifteen cells is worse than one that is
/// not capped at all.
///
/// A curated column never reaches here - it carries an explicit `kind` that `overlay::apply`
/// validates - so a dotted path is out of scope by construction.
pub fn is_ip_name(name: &str) -> bool {
    header_for(name).rsplit(' ').next() == Some("IP")
}

/// A percentage. The census over `specs/` finds thirteen distinct names - `percentage`,
/// `percentageComplete`, `progressPercentage`, `overallCompletionPercent`,
/// `fullSyncProgressPercent`, `compressionSavingPercent`, `dedupSavingPercent`,
/// `erasureCodingSavingPercent`, the three `*ReservationPercent`, `memoryThresholdPercent` and
/// `resilientCapacityWarningThresholdPercent` - and no false positive, so `contains` is safe
/// where `ends_with` would miss `percentageComplete`.
fn is_percent_name(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase()
        .contains("percent")
}

/// Epoch microseconds: a microsecond suffix (`Usecs`, and `InUsecs` which ends the same way),
/// **and** a time word, **and** no word that makes it a length rather than an instant.
///
/// The same suffix carries durations, which would render as an age from the epoch - "56 years
/// ago" for a VM that has been up an hour. Two shapes of those exist in `specs/`: a latency
/// (`controllerAvgIoLatencyUsecs`), which the time word already excludes, and
/// `hypervisorVmRunningTimeUsecs`, which says `time` and means a length. So the time word is
/// not enough on its own, and the four words below are the census's answer - every remaining
/// `*Usecs` name in `specs/` (`bootTimeUsecs`, `lastSyncTimeUsecs`, `scanTimestampUsecs`,
/// `startTimeInUsecs`, `endTimeInUsecs`, `creationTimestampInUsecs` and the
/// `*TimeStampInUsecs` family) is an instant.
fn is_micros_name(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
    last.ends_with("usecs")
        && ["time", "timestamp", "boot"]
            .iter()
            .any(|w| last.contains(w))
        && !["running", "elapsed", "total", "duration"]
            .iter()
            .any(|w| last.contains(w))
}

/// The first `Enum` column whose property name ends with `state`, `status`, `severity` or
/// `health` becomes the row's `Status` column. Only the first: a row has one tint, and
/// `powerState` must not fight `vmGuestCustomizationStatus` over it. `curated.toml`'s
/// `status_column` overrides this, and curated columns declaring `kind = "Status"` bypass it
/// entirely.
fn promote_status(cols: &mut [ColumnModel]) {
    if let Some(c) = cols
        .iter_mut()
        .find(|c| c.kind == "Enum" && is_status_name(&c.path))
    {
        c.kind = "Status".to_string();
    }
}

/// Case-insensitively, on the last dotted segment, so `platformData.connectivityStatus` counts
/// and `statusReportPath` does not.
fn is_status_name(path: &str) -> bool {
    let last = path.rsplit('.').next().unwrap_or(path).to_ascii_lowercase();
    ["state", "status", "severity", "health"]
        .iter()
        .any(|s| last.ends_with(s))
}

/// `powerState` → `POWER STATE`, `extId` → `EXT ID`, `minimumAHVVersion` → `MINIMUM AHV VERSION`.
pub fn header_for(prop: &str) -> String {
    let chars: Vec<char> = prop.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        let prev_lower =
            i > 0 && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());
        // `VMs`: an uppercase run ending in a plural `s` is one word, not an acronym boundary.
        let plural_s = chars.get(i + 1) == Some(&'s')
            && chars.get(i + 2).is_none_or(|n| !n.is_ascii_lowercase());
        let acronym_end = i > 0
            && chars[i - 1].is_ascii_uppercase()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase())
            && !plural_s;
        if c.is_ascii_uppercase() && (prev_lower || acronym_end) {
            out.push(' ');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A `$actions/` POST on a path with a placeholder: `needs_etag` starts `true`.
    fn entity_action(name: &str, path: &str) -> ActionModel {
        action(&json!({}), name, path, "Post", &json!({}), &json!({})).unwrap()
    }

    /// The ETag comes from `get_in` over the get path, so an action with more placeholders
    /// than the get path loses the flag while one with the same count keeps it.
    #[test]
    fn clear_unfetchable_etags_follows_the_get_path_s_placeholder_count() {
        let mut k = KindModel {
            id: "x.config.Thing".to_string(),
            get_path: Some("/x/v4.1/config/things/{a}".to_string()),
            actions: vec![
                entity_action("y", "/x/v4.1/config/things/{a}/$actions/y"),
                entity_action("z", "/x/v4.1/config/things/{a}/parts/{b}/$actions/z"),
            ],
            ..KindModel::default()
        };
        assert!(
            k.actions.iter().all(|a| a.needs_etag),
            "both start as entity actions"
        );
        clear_unfetchable_etags(&mut k);
        assert!(k.actions[0].needs_etag, "same count as the get path: kept");
        assert!(
            !k.actions[1].needs_etag,
            "one placeholder more than the get path: cleared"
        );

        // No get path at all: nothing can be fetched, so nothing may keep the flag.
        k.get_path = None;
        k.actions[0].needs_etag = true;
        clear_unfetchable_etags(&mut k);
        assert!(!k.actions[0].needs_etag);
    }

    /// One GET per shape of `data`, collected or refused. `lifecycle`'s `getConfig` is the
    /// singleton that got through: a list kind built on it answers
    /// `cannot decode response, expected a list, got object` on every poll, for ever.
    #[test]
    fn only_a_get_that_answers_with_a_collection_becomes_a_list_kind() {
        let schemas = json!({
            "ThingListApiResponse": {
                "properties": { "data": { "items": { "$ref": "#/components/schemas/Thing" } } }
            },
            "ThingApiResponse": {
                "properties": { "data": { "$ref": "#/components/schemas/Thing" } }
            },
            "NamesApiResponse": {
                "properties": { "data": { "type": "array", "items": { "type": "string" } } }
            }
        });
        let get = |wrapper: &str| {
            json!({ "responses": { "200": { "content": { "application/json": {
                "schema": { "$ref": format!("#/components/schemas/{wrapper}") }
            }}}}})
        };
        let paths = |path: &str, wrapper: &str| {
            let mut m = Map::new();
            m.insert(path.to_string(), json!({ "get": get(wrapper) }));
            m
        };
        let spec = SpecFile {
            namespace: "x".to_string(),
            version: "v4.1".to_string(),
            preview: false,
            path: std::path::PathBuf::new(),
            versions: vec!["v4.1".to_string()],
        };

        let listed = collect_list_kinds(
            &spec,
            &paths("/x/v4.1/config/things", "ThingListApiResponse"),
            &schemas,
        );
        assert_eq!(listed.len(), 1, "a collection is a list kind");
        assert_eq!(listed[0].schema, "Thing");

        let names = collect_list_kinds(
            &spec,
            &paths("/x/v4.1/config/names", "NamesApiResponse"),
            &schemas,
        );
        assert_eq!(
            names.len(),
            1,
            "an array of strings is still a collection, schema or no schema"
        );
        assert_eq!(names[0].schema, "", "and it has no item schema to name");

        let single = collect_list_kinds(
            &spec,
            &paths("/x/v4.1/resources/config", "ThingApiResponse"),
            &schemas,
        );
        assert!(
            single.is_empty(),
            "a singleton GET is not a list: {single:?}"
        );
    }
}
