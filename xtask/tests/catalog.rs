use std::path::{Path, PathBuf};

use xtask::catalog::{model::KindModel, spec};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn selects_latest_ga_version_per_namespace() {
    let files = spec::select_latest(&fixtures()).unwrap();
    let mini = files.iter().find(|f| f.namespace == "mini").unwrap();
    assert_eq!((mini.version.as_str(), mini.preview), ("v4.1", false));
    assert!(mini.path.ends_with("mini/v4.1.yaml"));
    assert_eq!(
        mini.versions,
        vec!["v4.1", "v4.0"],
        "GA versions newest first"
    );
    let pre = files
        .iter()
        .find(|f| f.namespace == "preview-only")
        .unwrap();
    assert_eq!((pre.version.as_str(), pre.preview), ("v4.0.b1", true));
    assert_eq!(
        pre.versions,
        vec!["v4.0.b1"],
        "a preview-only namespace lists itself"
    );
}

#[test]
fn version_predicates() {
    assert!(spec::is_ga("v4.3"));
    assert!(!spec::is_ga("v4.3.b1"));
    assert!(!spec::is_ga("v4.0.a3"));
    assert!(!spec::is_ga("4.3"));
}

#[test]
fn since_is_the_oldest_ga_spec_that_has_the_path() {
    use xtask::catalog::since::PathIndex;
    let files = spec::select_latest(&fixtures()).unwrap();
    let mini = files.iter().find(|f| f.namespace == "mini").unwrap();
    let index = PathIndex::load(mini).unwrap();
    assert_eq!(index.since("/mini/v4.1/config/widgets"), "v4.0");
    assert_eq!(index.since("/mini/v4.1/config/sprockets"), "v4.1");
    assert_eq!(
        index.since("/mini/v4.1/config/widgets/{widgetExtId}/gears"),
        "v4.1"
    );
    let pre = files
        .iter()
        .find(|f| f.namespace == "preview-only")
        .unwrap();
    let index = PathIndex::load(pre).unwrap();
    assert_eq!(
        index.since("/preview-only/v4.0.b1/config/things"),
        "v4.0.b1"
    );
}

#[test]
fn since_walks_older_versions_oldest_first() {
    use xtask::catalog::since::PathIndex;
    let files = spec::select_latest(&fixtures()).unwrap();
    let three = files.iter().find(|f| f.namespace == "three").unwrap();
    assert_eq!(three.versions, vec!["v4.2", "v4.1", "v4.0"]);
    let index = PathIndex::load(three).unwrap();
    assert_eq!(
        index.since("/three/v4.2/config/bolts"),
        "v4.0",
        "present in every version"
    );
    assert_eq!(
        index.since("/three/v4.2/config/nuts"),
        "v4.1",
        "introduced in the middle"
    );
    assert_eq!(
        index.since("/three/v4.2/config/washers"),
        "v4.2",
        "new in the chosen version"
    );
    assert_eq!(
        index.since("/three/v4.2/config/bolts/{extId}"),
        "v4.0",
        "v4.0 spells the same endpoint {{boltId}}: a renamed parameter is not a new endpoint"
    );
}

fn mini() -> Vec<KindModel> {
    let files = spec::select_latest(&fixtures()).unwrap();
    let f = files.iter().find(|f| f.namespace == "mini").unwrap();
    let doc = spec::load(&f.path).unwrap();
    xtask::catalog::parse::parse_namespace(f, &doc).unwrap()
}

/// No pages: what the render tests that are about the kinds pass for the table they do not
/// exercise. The `&[]` and the `0` beside it are the empty nav tree.
fn no_pages() -> xtask::catalog::pages::PagesFile {
    xtask::catalog::pages::parse("").unwrap()
}

fn find_mut<'a>(kinds: &'a mut [KindModel], id: &str) -> &'a mut KindModel {
    kinds
        .iter_mut()
        .find(|k| k.id == id)
        .unwrap_or_else(|| panic!("no kind {id}"))
}

fn find<'a>(kinds: &'a [KindModel], id: &str) -> &'a KindModel {
    kinds.iter().find(|k| k.id == id).unwrap_or_else(|| {
        panic!(
            "no kind {id} in {:?}",
            kinds.iter().map(|k| &k.id).collect::<Vec<_>>()
        )
    })
}

#[test]
fn widget_kind_is_parsed() {
    let kinds = mini();
    assert_eq!(
        kinds.len(),
        5,
        "{:?}",
        kinds.iter().map(|k| &k.id).collect::<Vec<_>>()
    );
    let w = find(&kinds, "mini.config.Widget");
    assert_eq!(w.namespace, "mini");
    assert_eq!(w.version, "v4.1");
    assert_eq!(w.list_path, "/mini/v4.1/config/widgets");
    assert_eq!(
        w.get_path.as_deref(),
        Some("/mini/v4.1/config/widgets/{extId}")
    );
    assert_eq!(w.schema, "mini.v4.1.config.Widget");
    assert!(w.list_params.page && w.list_params.limit && w.list_params.filter);
    assert!(!w.list_params.select && !w.list_params.required);
    assert_eq!(w.display, "Widgets");
    assert_eq!(w.aliases, vec!["widgets", "widget"]);
    assert_eq!(w.category, "Mini");
    assert_eq!(w.poll_secs, 30);
    assert!(!w.preview);
}

#[test]
fn stats_list_with_required_param_is_flagged() {
    let kinds = mini();
    let s = find(&kinds, "mini.stats.WidgetStats");
    assert!(s.list_params.required);
    assert_eq!(s.get_path, None);
}

#[test]
fn oneof_wrapped_list_resolves_schema_and_strips_r_segments() {
    let kinds = mini();
    let s = find(&kinds, "mini.config.Sprocket");
    assert_eq!(s.schema, "mini.v4.r0.b1.config.Sprocket");
    assert_eq!(s.list_path, "/mini/v4.1/config/sprockets");
    assert_eq!(s.display, "Sprockets");
}

#[test]
fn pluralization_helpers() {
    use xtask::catalog::parse::{pluralize, singular};
    assert_eq!(pluralize("Policy"), "Policies");
    assert_eq!(pluralize("Key"), "Keys");
    assert_eq!(pluralize("Address"), "Addresses");
    assert_eq!(pluralize("Stats"), "Stats");
    assert_eq!(pluralize("Vm"), "Vms");
    assert_eq!(singular("vms").as_deref(), Some("vm"));
    assert_eq!(singular("policies").as_deref(), Some("policy"));
    assert_eq!(singular("addresses").as_deref(), Some("address"));
    assert_eq!(singular("status"), None);
    assert_eq!(singular("stats"), None);
}

#[test]
fn gear_is_a_sub_resource_of_widget() {
    let kinds = mini();
    let g = find(&kinds, "mini.config.Gear");
    assert_eq!(g.list_path, "/mini/v4.1/config/widgets/{widgetExtId}/gears");
    assert_eq!(g.parent.as_deref(), Some("mini.config.Widget"));
    assert_eq!(g.get_path, None);
    assert_eq!(g.aliases, vec!["gears", "gear"]);
    let w = find(&kinds, "mini.config.Widget");
    assert_eq!(w.parent, None);
}

#[test]
fn widget_actions_follow_etag_rules() {
    let kinds = mini();
    let w = find(&kinds, "mini.config.Widget");
    let names: Vec<&str> = w.actions.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["create", "delete", "repaint", "spin", "update"]);
    let by = |n: &str| w.actions.iter().find(|a| a.name == n).unwrap();
    // `(needs_etag, takes_body, needs_body, method)`; the fixture declares its bodies without
    // `required: true`, so they are taken but not needed.
    let shape = |n: &str| {
        let a = by(n);
        (a.needs_etag, a.takes_body, a.needs_body, a.method.as_str())
    };
    assert_eq!(shape("create"), (false, true, false, "Post"));
    assert_eq!(shape("update"), (true, true, false, "Put"));
    assert_eq!(shape("delete"), (true, false, false, "Delete"));
    let spin = by("spin");
    assert_eq!(shape("spin"), (true, false, false, "Post"));
    assert_eq!(spin.path, "/mini/v4.1/config/widgets/{extId}/$actions/spin");
    assert_eq!(spin.roles, vec!["Administrator"]);
    assert_eq!(by("create").path, "/mini/v4.1/config/widgets");
    let repaint = by("repaint");
    assert_eq!(
        repaint.path,
        "/mini/v4.1/config/widgets/{widgetExtId}/$actions/repaint"
    );
    assert!(repaint.needs_etag);
    let g = find(&kinds, "mini.config.Gear");
    assert!(g.actions.is_empty());
}

#[test]
fn create_never_needs_an_etag_even_if_declared() {
    let kinds = mini();
    let s = find(&kinds, "mini.config.Sprocket");
    let create = s.actions.iter().find(|a| a.name == "create").unwrap();
    assert!(!create.needs_etag);
    assert!(
        create.takes_body && !create.needs_body,
        "declared without `required: true`"
    );
}

#[test]
fn fallback_columns_are_ordered_by_kind() {
    let kinds = mini();
    let w = find(&kinds, "mini.config.Widget");
    let cols: Vec<(&str, &str, &str)> = w
        .fallback_columns
        .iter()
        .map(|c| (c.header.as_str(), c.path.as_str(), c.kind.as_str()))
        .collect();
    assert_eq!(
        cols,
        vec![
            ("NAME", "name", "Text"),
            ("POWER STATE", "powerState", "Status"),
            ("SYNC STATUS", "syncStatus", "Enum"),
            ("CREATE TIME", "createTime", "Timestamp"),
            ("CREATION TIMESTAMP", "creationTimestamp", "Timestamp"),
            ("CLUSTER", "cluster.extId", "Reference"),
            ("RACK", "rack.uuid", "Reference"),
            ("OWNER UUID", "ownerUuid", "Reference"),
            ("RETENTION TIME SECONDS", "retentionTimeSeconds", "Duration"),
            ("MEMORY SIZE BYTES", "memorySizeBytes", "Bytes"),
            ("IS ENABLED", "isEnabled", "Bool"),
            ("EXTERNAL IP", "externalIp", "Ip"),
            ("OWNERSHIP", "ownership", "Text"),
            ("PROGRESS PERCENTAGE", "progressPercentage", "Percent"),
            ("BOOT TIME USECS", "bootTimeUsecs", "Micros"),
            (
                "CONTROLLER AVG IO LATENCY USECS",
                "controllerAvgIoLatencyUsecs",
                "Text"
            ),
            (
                "HYPERVISOR VM RUNNING TIME USECS",
                "hypervisorVmRunningTimeUsecs",
                "Text"
            ),
            (
                "IS FORCE RESET PASSWORD ENABLED",
                "isForceResetPasswordEnabled",
                "Bool"
            ),
            ("GEARS", "gears", "Count"),
        ]
    );
}

/// The three new column kinds, each earning its place on a width or on a Nutanix unit no
/// existing kind renders. `ownership` is the negative case: its lowercase `ip` continues a
/// word rather than starting one, so it is text, not an address.
#[test]
fn the_three_new_column_kinds_are_derived_from_the_names() {
    let kinds = mini();
    assert_eq!(column(&kinds, "EXTERNAL IP").kind, "Ip");
    assert_eq!(column(&kinds, "OWNERSHIP").kind, "Text");
    assert_eq!(column(&kinds, "PROGRESS PERCENTAGE").kind, "Percent");
    assert_eq!(column(&kinds, "BOOT TIME USECS").kind, "Micros");
    // The same suffix is used for durations, which are not instants and would read as an age
    // from the epoch - "56 years ago". A latency has no time word at all; a running time has
    // one and is still a length, so the rule needs both halves.
    assert_eq!(
        column(&kinds, "CONTROLLER AVG IO LATENCY USECS").kind,
        "Text"
    );
    assert_eq!(
        column(&kinds, "HYPERVISOR VM RUNNING TIME USECS").kind,
        "Text"
    );
}

/// The address rule, on the helper rather than through a fixture: it is the last **word** of a
/// property name, so a lowercase `ip` that continues a word is not an address, and neither is
/// an `ip` that starts one.
#[test]
fn the_address_rule_matches_a_whole_camel_word() {
    use xtask::catalog::parse::is_ip_name;
    for yes in ["externalIp", "externalIP", "cvmIp", "ip"] {
        assert!(is_ip_name(yes), "{yes}");
    }
    for no in [
        "ownership",
        "membership",
        "description",
        "masterVip",
        "ipConfig",
    ] {
        assert!(!is_ip_name(no), "{no}");
    }
}

/// A column of `kinds`' widget, by header.
fn column<'a>(kinds: &'a [KindModel], header: &str) -> &'a xtask::catalog::model::ColumnModel {
    let w = find(kinds, "mini.config.Widget");
    w.fallback_columns
        .iter()
        .find(|c| c.header == header)
        .unwrap_or_else(|| {
            panic!(
                "no {header} column in {:?}",
                w.fallback_columns
                    .iter()
                    .map(|c| (&c.header, &c.kind))
                    .collect::<Vec<_>>()
            )
        })
}

/// Nutanix writes one literal UUID pattern on every identifier property - 326 occurrences in
/// `specs/vmm/v4.3.yaml` alone - so the schema says which strings are references and no name
/// list has to be kept up to date. With the warm-up they then resolve to names.
#[test]
fn a_uuid_patterned_string_is_a_reference() {
    let kinds = mini();
    assert_eq!(column(&kinds, "OWNER UUID").kind, "Reference");
    // And it lands in the reference bucket, which sits before the plain scalars.
    let w = find(&kinds, "mini.config.Widget");
    let refs: Vec<&str> = w
        .fallback_columns
        .iter()
        .filter(|c| c.kind == "Reference")
        .map(|c| c.path.as_str())
        .collect();
    assert!(refs.contains(&"ownerUuid"), "{refs:?}");
}

/// The same reference twice, once as an expanded object and once as a flat string, is one
/// column. `networking.config.FloatingIp` has `vpc.extId` beside `vpcReference` and
/// `externalSubnet.extId` beside `externalSubnetReference`; `networking.config.BgpSession` has
/// it twice over.
#[test]
fn references_are_de_duplicated_by_their_base() {
    use xtask::catalog::parse::reference_key;
    for (a, b) in [
        ("vpc.extId", "vpcReference"),
        ("vm.extId", "vmExtId"),
        ("localGateway.extId", "localGatewayReference"),
        ("cluster.uuid", "clusterExtId"),
        ("owner.extId", "ownerUuid"),
    ] {
        assert_eq!(reference_key(a), reference_key(b), "{a} vs {b}");
    }
    assert_ne!(reference_key("createdBy"), reference_key("updatedBy"));
    assert_ne!(reference_key("vpc.extId"), reference_key("host.extId"));

    // End to end: `cluster.extId` and `clusterReference` are one column, and it is the dotted
    // one - the object form is what a newer API version populates.
    let kinds = mini();
    let w = find(&kinds, "mini.config.Widget");
    let clusters: Vec<&str> = w
        .fallback_columns
        .iter()
        .filter(|c| reference_key(&c.path) == "cluster")
        .map(|c| c.path.as_str())
        .collect();
    assert_eq!(clusters, vec!["cluster.extId"]);
}

/// A write-only property, and a string whose name says it is a secret, are never columns. The
/// rule is `contains` and not `ends_with` - `secretAccessKey` is neither `writeOnly` nor
/// suffixed - and it is applied to strings only, so `isForceResetPasswordEnabled` survives as
/// the legitimate `Bool` column it is.
#[test]
fn secrets_and_write_only_properties_are_never_columns() {
    let kinds = mini();
    let w = find(&kinds, "mini.config.Widget");
    let paths: Vec<&str> = w.fallback_columns.iter().map(|c| c.path.as_str()).collect();
    for gone in ["password", "targetSecret", "secretAccessKey", "quickMode"] {
        assert!(
            !paths.contains(&gone),
            "{gone} is still a column: {paths:?}"
        );
    }
    assert!(
        paths.contains(&"isForceResetPasswordEnabled"),
        "a Bool flag is not a secret: {paths:?}"
    );
    assert_eq!(
        column(&kinds, "IS FORCE RESET PASSWORD ENABLED").kind,
        "Bool"
    );
}

/// The marker list is the recorder's, so the two refusals cannot drift: what the leak scan
/// takes out of a fixture is what the generator refuses to make a column of. `privateKey` and
/// `reclaimToken` are the names the catalog census found that `password`, `secret` and
/// `passphrase` alone would still have shipped.
#[test]
fn the_secret_markers_are_the_recorders_and_the_exceptions_are_kept() {
    use nutsh_catalog::secret::is_secret_name;
    for yes in [
        "privateKey",
        "privateKeyPassphrase",
        "password",
        "secretAccessKey",
        "clientSecret",
        "targetSecret",
        "reclaimToken",
        "cloudInitScript",
    ] {
        assert!(is_secret_name(yes), "{yes}");
    }
    // Each of these describes a secret instead of being one, and stays an ordinary column.
    for no in [
        "claimTokenExtId",
        "accessKeyName",
        "apiCredentialStatus",
        "credentialIssuer",
        "authorizationPolicyType",
        "description",
        "key",
    ] {
        assert!(!is_secret_name(no), "{no}");
    }
    // The flags *around* a secret are booleans, so the name rule never gets to judge them:
    // `is_secret_property` asks it of `string` properties only, and these stay the columns
    // they are.
    for flag in [
        "hasPrivateKey",
        "shouldValidateAdCredential",
        "isForceResetPasswordEnabled",
    ] {
        let schemas = serde_json::json!({
            "x.v4.0.config.Thing": {
                "type": "object",
                "properties": {flag: {"type": "boolean"}}
            }
        });
        let cols = xtask::catalog::parse::fallback_columns(&schemas, "x.v4.0.config.Thing");
        let paths: Vec<&str> = cols.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec![flag]);
    }
}

/// `projectExtId` appears on 31 nav kinds, the nav marks *Projects* as `not in the v4 API`
/// (`listProjects` exists only in the beta `specs/multidomain/v4.4.b1.yaml` and the catalog
/// pins `multidomain` at v4.3), and it was empty on every lab row inspected. A column that can
/// never resolve and is always empty is not a column.
#[test]
fn project_ext_id_is_skipped_everywhere() {
    let kinds = mini();
    let w = find(&kinds, "mini.config.Widget");
    assert!(
        !w.fallback_columns.iter().any(|c| c.path == "projectExtId"),
        "{:?}",
        w.fallback_columns
    );
}

/// A timestamp the spec forgot to mark `format: date-time`.
/// `security.config.KeyManagementServer.creationTimestamp` is the real one. `cell::render`
/// falls back to the raw text when the value does not parse as RFC 3339, so a string named
/// `startTime` that holds `08:00` degrades to what it does today.
#[test]
fn a_time_named_string_is_a_timestamp() {
    let kinds = mini();
    assert_eq!(column(&kinds, "CREATION TIMESTAMP").kind, "Timestamp");
}

/// `promote_status` running after `truncate(MAX_FALLBACK_COLUMNS)` would never promote an enum
/// that carries the row tint but lands at position 21. Promote first, truncate second.
#[test]
fn the_status_is_promoted_before_the_columns_are_truncated() {
    let mut props = serde_json::Map::new();
    props.insert("name".into(), serde_json::json!({"type": "string"}));
    // Twenty-two plain scalars, so the one status-shaped enum lands past the cap.
    for i in 0..22 {
        props.insert(
            format!("field{i:02}"),
            serde_json::json!({"type": "string"}),
        );
    }
    props.insert(
        "lifecycleState".into(),
        serde_json::json!({"type": "string", "enum": ["ACTIVE", "GONE"]}),
    );
    let schemas = serde_json::json!({
        "x.v4.0.config.Thing": {"type": "object", "properties": props}
    });
    let cols = xtask::catalog::parse::fallback_columns(&schemas, "x.v4.0.config.Thing");
    assert!(cols.len() <= 20, "the cap still holds: {}", cols.len());
    let status: Vec<&str> = cols
        .iter()
        .filter(|c| c.kind == "Status")
        .map(|c| c.path.as_str())
        .collect();
    assert_eq!(status, vec!["lifecycleState"], "{cols:?}");
}

/// A row has one tint, so only the first status-shaped enum is promoted; the second stays an
/// ordinary `Enum` and is droppable like any other column.
#[test]
fn the_first_status_shaped_enum_is_promoted_and_the_second_is_not() {
    let kinds = mini();
    assert_eq!(column(&kinds, "POWER STATE").kind, "Status");
    assert_eq!(column(&kinds, "SYNC STATUS").kind, "Enum");
}

/// An integer of seconds is a duration, beside the existing `…Bytes` rule.
#[test]
fn integer_seconds_become_a_duration() {
    let kinds = mini();
    assert_eq!(column(&kinds, "RETENTION TIME SECONDS").kind, "Duration");
    assert_eq!(column(&kinds, "MEMORY SIZE BYTES").kind, "Bytes");
}

#[test]
fn status_column_overrides_the_heuristic_and_empty_promotes_none() {
    use xtask::catalog::overlay;
    let mut kinds = mini();
    let o =
        overlay::parse("[kinds.\"mini.config.Widget\"]\nstatus_column = \"syncStatus\"\n").unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    assert_eq!(column(&kinds, "SYNC STATUS").kind, "Status");
    assert_eq!(column(&kinds, "POWER STATE").kind, "Enum", "demoted");

    let mut kinds = mini();
    let none = overlay::parse("[kinds.\"mini.config.Widget\"]\nstatus_column = \"\"\n").unwrap();
    overlay::apply(&none, &mut kinds).unwrap();
    assert!(
        find(&kinds, "mini.config.Widget")
            .fallback_columns
            .iter()
            .all(|c| c.kind != "Status")
    );

    let mut kinds = mini();
    let bad = overlay::parse("[kinds.\"mini.config.Widget\"]\nstatus_column = \"nope\"\n").unwrap();
    let err = overlay::apply(&bad, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("nope"), "{err}");

    // Beside curated columns the TUI never reads the fallback ones, so the override is refused
    // rather than accepted and ignored.
    let mut kinds = mini();
    let beside = overlay::parse(
        "[kinds.\"mini.config.Widget\"]\nstatus_column = \"syncStatus\"\ncolumns = [{ header = \"NAME\", path = \"name\", kind = \"Text\" }]\n",
    )
    .unwrap();
    let err = overlay::apply(&beside, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("no effect beside curated columns"), "{err}");
}

#[test]
fn status_role_overrides_are_normalised_sorted_and_checked() {
    use xtask::catalog::overlay;
    let mut kinds = mini();
    let o = overlay::parse(
        "[kinds.\"mini.config.Widget\".status]\nQUEUED = \"Muted\"\n\"in-sync\" = \"Ok\"\n",
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    assert_eq!(
        find(&kinds, "mini.config.Widget").status_roles,
        vec![
            ("IN_SYNC".to_string(), "Ok".to_string()),
            ("QUEUED".to_string(), "Muted".to_string()),
        ],
        "normalised and sorted"
    );

    let mut kinds = mini();
    let bad = overlay::parse("[kinds.\"mini.config.Widget\".status]\nQUEUED = \"Nope\"\n").unwrap();
    let err = overlay::apply(&bad, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("Nope"), "{err}");

    // Two spellings that fold to one word are a coin toss over which survives, so they are an
    // error, not a dedup.
    let mut kinds = mini();
    let clash = overlay::parse(
        "[kinds.\"mini.config.Widget\".status]\n\"IN-SYNC\" = \"Ok\"\nIN_SYNC = \"Error\"\n",
    )
    .unwrap();
    let err = overlay::apply(&clash, &mut kinds).unwrap_err().to_string();
    assert!(
        err.contains("\"IN-SYNC\"")
            && err.contains("\"IN_SYNC\"")
            && err.contains("both normalise to \"IN_SYNC\""),
        "{err}"
    );
}

#[test]
fn render_emits_status_roles_and_the_new_column_kinds() {
    use xtask::catalog::{model::NamespaceModel, render};
    let mut kinds = mini();
    kinds[0].status_roles = vec![("QUEUED".into(), "Muted".into())];
    // Without a form anywhere, `Field` and `FieldType` stay out of the import line; the
    // with-form line is pinned by `render_emits_form_fields_and_imports_their_types`.
    for a in kinds.iter_mut().flat_map(|k| k.actions.iter_mut()) {
        a.form.clear();
    }
    let ns = vec![NamespaceModel {
        name: "mini".into(),
        version: "v4.1".into(),
        preview: false,
        versions: vec!["v4.1".into()],
    }];
    let src = render::render(&ns, &kinds, &[], 0, &no_pages(), 0).unwrap();
    assert!(
        src.contains(
            "use crate::{Action, ActionReturn, Column, ColumnKind, ConfirmKind, Danger, Kind, ListParams, Method, Namespace, NavGroup, PageDef, RateLimit, Role};"
        ),
        "{src}"
    );
    assert!(
        src.contains("status_roles: &[(\"QUEUED\", Role::Muted)],"),
        "{src}"
    );
    assert!(src.contains("kind: ColumnKind::Status }"), "{src}");
    assert!(src.contains("kind: ColumnKind::Duration }"), "{src}");

    kinds[0].status_roles = vec![("QUEUED".into(), "Nope".into())];
    assert!(
        render::render(&ns, &kinds, &[], 0, &no_pages(), 0).is_err(),
        "an unknown role fails generation"
    );
}

/// A collection the spec describes as `data: {}` and nothing more is still a collection, and
/// the kind built on it gets a hard-coded pair of columns. A dim eight-character stub beats a
/// 36-character UUID while it waits for curation, which is what `microseg.config.policies` has.
#[test]
fn schemaless_kind_gets_name_and_ext_id() {
    let files = spec::select_latest(&fixtures()).unwrap();
    let f = files
        .iter()
        .find(|f| f.namespace == "preview-only")
        .unwrap();
    let doc = spec::load(&f.path).unwrap();
    let kinds = xtask::catalog::parse::parse_namespace(f, &doc).unwrap();
    let t = find(&kinds, "preview-only.config.things");
    assert_eq!(t.schema, "");
    assert!(t.preview);
    assert_eq!(t.display, "Things");
    let cols: Vec<(&str, &str, &str)> = t
        .fallback_columns
        .iter()
        .map(|c| (c.header.as_str(), c.path.as_str(), c.kind.as_str()))
        .collect();
    assert_eq!(
        cols,
        vec![("NAME", "name", "Text"), ("EXT ID", "extId", "Reference")]
    );
}

/// The schemas one namespace declares, as `overlay::check` wants them: keyed by namespace,
/// each value the `components.schemas` object of that namespace's spec.
fn schema_map(
    pairs: &[(&str, serde_json::Value)],
) -> std::collections::BTreeMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(ns, v)| ((*ns).to_string(), v.clone()))
        .collect()
}

/// A kind exactly as `collect_list_kinds` would leave it, so a check test needs no fixture
/// spec: the checks read `id`, `namespace`, `schema`, `columns`, `fallback_columns`,
/// `list_path`, `parent`, `list_params`, `name_path` and `warm`, and nothing else.
fn checkable(id: &str, schema: &str, list_path: &str) -> KindModel {
    KindModel {
        id: id.to_string(),
        namespace: "mini".to_string(),
        version: "v4.1".to_string(),
        schema: schema.to_string(),
        list_path: list_path.to_string(),
        ext_id_key: "extId".to_string(),
        ..KindModel::default()
    }
}

/// The check that makes §6.3's gate enforceable: a hand-written column is otherwise validated
/// only for its `kind`, so a typo ships a column of dashes nobody notices until a run against a Prism Central.
#[test]
fn a_curated_path_that_resolves_against_nothing_fails_generation() {
    let schemas = schema_map(&[(
        "mini",
        serde_json::json!({
            "mini.v4.1.config.Thing": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "cluster": {"type": "object", "properties": {"extId": {"type": "string"}}},
                    "nics": {
                        "type": "array",
                        "items": {"$ref": "#/components/schemas/mini.v4.1.config.Nic"}
                    },
                    "access": {
                        "oneOf": [
                            {"$ref": "#/components/schemas/mini.v4.1.config.Azure"},
                            {"$ref": "#/components/schemas/mini.v4.1.config.Kmip"}
                        ]
                    }
                }
            },
            "mini.v4.1.config.Nic": {
                "type": "object",
                "properties": {"ipv4": {"type": "object", "properties": {"value": {"type": "string"}}}}
            },
            "mini.v4.1.config.Azure": {"type": "object", "properties": {"endpointUrl": {"type": "string"}}},
            "mini.v4.1.config.Kmip": {"type": "object", "properties": {"caName": {"type": "string"}}}
        }),
    )]);
    let good = |path: &str| {
        let mut k = checkable(
            "mini.config.Thing",
            "mini.v4.1.config.Thing",
            "/mini/v4.1/config/things",
        );
        k.columns = vec![xtask::catalog::model::ColumnModel {
            header: "H".into(),
            path: path.into(),
            kind: "Text".into(),
        }];
        xtask::catalog::overlay::check(&[k], &schemas)
    };
    for path in [
        "name",
        "cluster.extId",
        "nics[].ipv4.value",
        // Both arms of a `oneOf` resolve: `security.config.KeyManagementServer` puts its
        // endpoint on one and its CA on the other.
        "access.endpointUrl",
        "access.caName",
    ] {
        good(path).unwrap_or_else(|e| panic!("{path} should resolve: {e}"));
    }
    for path in [
        "nmae",
        "cluster.uuid",
        "nics[].ipv6.value",
        "access.nope",
        "name.deeper",
    ] {
        let e = good(path).unwrap_err();
        assert!(
            e.to_string().contains("resolves against nothing"),
            "{path}: {e}"
        );
    }
}

/// The schemas a detail check needs: an object with a polymorphic property declared the way
/// `vmm.v4.3.ahv.config.Vm` declares `bootConfig` - a `oneOf` of two named arms, with
/// `$objectType` as an inline enum beside it.
fn detail_schemas() -> std::collections::BTreeMap<String, serde_json::Value> {
    schema_map(&[(
        "mini",
        serde_json::json!({
            "mini.v4.1.config.Thing": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "bootConfig": {
                        "properties": {
                            "$objectType": {
                                "type": "string",
                                "enum": ["mini.v4.config.LegacyBoot", "mini.v4.config.UefiBoot"]
                            }
                        },
                        "oneOf": [
                            {"$ref": "#/components/schemas/mini.v4.1.config.LegacyBoot"},
                            {"$ref": "#/components/schemas/mini.v4.1.config.UefiBoot"}
                        ]
                    }
                }
            },
            "mini.v4.1.config.LegacyBoot": {
                "type": "object",
                "properties": {"bootOrder": {"type": "array", "items": {"type": "string"}}}
            },
            "mini.v4.1.config.UefiBoot": {
                "type": "object",
                "properties": {"isSecureBootEnabled": {"type": "boolean"}}
            }
        }),
    )])
}

/// A kind with one detail section, for the checks to chew on.
fn with_detail(
    title: &str,
    when: Option<&str>,
    fields: &[(&str, &str, &str, Option<&str>)],
) -> KindModel {
    use xtask::catalog::model::{DetailFieldModel, DetailSectionModel};
    let mut k = checkable(
        "mini.config.Thing",
        "mini.v4.1.config.Thing",
        "/mini/v4.1/config/things",
    );
    k.detail = vec![DetailSectionModel {
        title: title.to_string(),
        when: when.map(str::to_string),
        fields: fields
            .iter()
            .map(|(label, path, kind, when)| DetailFieldModel {
                label: (*label).to_string(),
                path: (*path).to_string(),
                kind: (*kind).to_string(),
                when: when.map(str::to_string),
            })
            .collect(),
    }];
    k
}

/// The whole point of check 5's `$objectType` rule: the *literal* is validated, which a path
/// check can never do. `UefiBoot` is an arm of `bootConfig`, `UefiBoott` is not, and `nope` is
/// not a property at all.
#[test]
fn an_object_type_when_is_checked_against_the_arms_of_its_own_property() {
    let schemas = detail_schemas();
    let check = |when: &str| {
        let k = with_detail(
            "UEFI boot",
            Some(when),
            &[(
                "Secure boot",
                "bootConfig.isSecureBootEnabled",
                "Bool",
                None,
            )],
        );
        xtask::catalog::overlay::check(&[k], &schemas)
    };
    check("bootConfig.$objectType = UefiBoot").expect("UefiBoot is an arm");
    check("bootConfig.$objectType = LegacyBoot").expect("LegacyBoot is an arm");
    // Case-insensitive, so a `when` may be written the way a person writes it.
    check("bootConfig.$objectType = uefiboot").expect("compared case-insensitively");

    let e = check("bootConfig.$objectType = UefiBoott")
        .unwrap_err()
        .to_string();
    assert!(e.contains("UefiBoott"), "{e}");
    assert!(
        e.contains("LegacyBoot"),
        "the arm list is in the message: {e}"
    );

    let e = check("nope.$objectType = UefiBoot")
        .unwrap_err()
        .to_string();
    assert!(e.contains("nope"), "{e}");

    // A `$objectType` presence test is true of every object a Prism Central emits, so it says
    // nothing; it is refused rather than shipped as a dead condition.
    let e = check("bootConfig.$objectType").unwrap_err().to_string();
    assert!(e.contains("$objectType"), "{e}");
}

/// Two schemas the `$objectType` rule has to answer for beyond `bootConfig`: a `oneOf` whose
/// arms share a short name, and a top-level schema that is itself polymorphic.
fn variant_schemas() -> std::collections::BTreeMap<String, serde_json::Value> {
    schema_map(&[(
        "mini",
        serde_json::json!({
            // Two arms, one short name. Not a hypothetical: `disks[].backingInfo` is tagged
            // `vmm.v4.r0.b1.ahv.config.VmDisk` in one committed fixture and
            // `vmm.v4.ahv.config.VmDisk` in the next, so a schema listing both spellings is the
            // shape the short-name comparison cannot tell apart.
            "mini.v4.1.config.Boxed": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "backing": {
                        "properties": {"$objectType": {"type": "string"}},
                        "oneOf": [
                            {"$ref": "#/components/schemas/mini.v4.1.config.VmDisk"},
                            {"$ref": "#/components/schemas/mini.v4.1.r0.b1.config.VmDisk"}
                        ]
                    }
                }
            },
            "mini.v4.1.config.VmDisk": {
                "type": "object",
                "properties": {"diskSizeBytes": {"type": "integer"}}
            },
            "mini.v4.1.r0.b1.config.VmDisk": {
                "type": "object",
                "properties": {"diskSizeBytes": {"type": "integer"}}
            },
            // Polymorphic at the root: a bare `$objectType` has an empty prefix, which resolves
            // trivially, so the arms of the entity's own schema are what decide.
            "mini.v4.1.config.Shape": {
                "properties": {
                    "$objectType": {"type": "string"},
                    "label": {"type": "string"}
                },
                "oneOf": [
                    {"$ref": "#/components/schemas/mini.v4.1.config.Circle"},
                    {"$ref": "#/components/schemas/mini.v4.1.config.Square"}
                ]
            },
            "mini.v4.1.config.Circle": {"type": "object", "properties": {"radius": {"type": "integer"}}},
            "mini.v4.1.config.Square": {"type": "object", "properties": {"side": {"type": "integer"}}}
        }),
    )])
}

/// A `with_detail` over another schema, with one field and one section `when`.
fn with_detail_in(schema: &str, when: &str, path: &str) -> KindModel {
    let mut k = with_detail("Variant", Some(when), &[("Field", path, "Text", None)]);
    k.schema = schema.to_string();
    k
}

/// A literal that matches **two** arms is refused: the short name is all a wire tag carries, so
/// two arms spelled apart only by their version segments leave a `when` with no way to say which
/// it meant. `variant_names` therefore returns one entry per arm, repeats included - a
/// de-duplicating walk would make this case unreachable and the check a comment.
#[test]
fn a_when_that_matches_two_arms_of_one_short_name_is_refused() {
    let schemas = variant_schemas();
    let check = |when: &str| {
        let k = with_detail_in("mini.v4.1.config.Boxed", when, "backing.diskSizeBytes");
        xtask::catalog::overlay::check(&[k], &schemas)
    };
    let e = check("backing.$objectType = VmDisk")
        .unwrap_err()
        .to_string();
    assert!(e.contains("matches 2 arms"), "{e}");
    assert!(e.contains("backing"), "{e}");
    // And a literal that matches neither still says what the arms are, once each: a repeat in a
    // message is noise, and only the counting needs the repeats.
    let e = check("backing.$objectType = Nope").unwrap_err().to_string();
    assert!(e.contains("arms: VmDisk)"), "listed once, not twice: {e}");
}

/// A bare `$objectType` has an empty prefix, and an empty path resolves against anything - so
/// the path half of the check says nothing here and the arm half is the whole of it.
#[test]
fn a_root_level_object_type_is_checked_against_the_entitys_own_arms() {
    let schemas = variant_schemas();
    let polymorphic = |when: &str| {
        let k = with_detail_in("mini.v4.1.config.Shape", when, "label");
        xtask::catalog::overlay::check(&[k], &schemas)
    };
    polymorphic("$objectType = Circle").expect("Circle is an arm of the entity's own schema");
    let e = polymorphic("$objectType = Blob").unwrap_err().to_string();
    assert!(e.contains("Circle, Square"), "{e}");

    // A schema that is not polymorphic has no arms at all, so **every** literal is refused -
    // which is what stops a bare `$objectType = Thing` from being accepted on the strength of an
    // empty prefix that resolves trivially.
    let k = with_detail_in("mini.v4.1.config.Thing", "$objectType = Thing", "name");
    let e = xtask::catalog::overlay::check(&[k], &detail_schemas())
        .unwrap_err()
        .to_string();
    assert!(e.contains("not polymorphic"), "{e}");
}

/// Checks 1 to 4 and 6, each with the one thing it is for.
#[test]
fn the_detail_checks_refuse_a_bad_section() {
    let schemas = detail_schemas();
    let one = |k: KindModel| xtask::catalog::overlay::check(&[k], &schemas);

    // 1: a path that resolves against nothing.
    let e = one(with_detail(
        "Identity",
        None,
        &[("Name", "nmae", "Text", None)],
    ))
    .unwrap_err()
    .to_string();
    assert!(e.contains("nmae"), "{e}");

    // 2: a label wider than the pane's label column.
    let e = one(with_detail(
        "Identity",
        None,
        &[("A nineteen char lbl", "name", "Text", None)],
    ))
    .unwrap_err()
    .to_string();
    assert!(e.contains("19"), "{e}");

    // 3: a column kind the vocabulary does not have.
    let e = one(with_detail(
        "Identity",
        None,
        &[("Name", "name", "Txt", None)],
    ))
    .unwrap_err()
    .to_string();
    assert!(e.contains("Txt"), "{e}");

    // 4: a section with no fields.
    let e = one(with_detail("Identity", None, &[]))
        .unwrap_err()
        .to_string();
    assert!(e.contains("Identity"), "{e}");

    // 6: two sections with the same title.
    let mut k = with_detail("Identity", None, &[("Name", "name", "Text", None)]);
    k.detail.push(k.detail[0].clone());
    let e = one(k).unwrap_err().to_string();
    assert!(e.contains("Identity"), "{e}");

    // A field's own `when` goes through the same evaluator as a section's.
    one(with_detail(
        "Identity",
        None,
        &[("Name", "name", "Text", Some("!bootConfig"))],
    ))
    .expect("a field `when` over a resolvable path");
    let e = one(with_detail(
        "Identity",
        None,
        &[("Name", "name", "Text", Some("nope.deeper"))],
    ))
    .unwrap_err()
    .to_string();
    assert!(e.contains("nope.deeper"), "{e}");
}

/// `ui::render_rows` computes `table::status_index` over the columns it *drew*, and
/// `table::columns` slices to `DEFAULT_COLUMNS` unless `w` is held, so a `Status` at index 6
/// tints nothing and is silently dead.
#[test]
fn a_curated_status_past_the_sixth_column_fails_generation() {
    let mut props = serde_json::Map::new();
    for n in ["a", "b", "c", "d", "e", "f", "g"] {
        props.insert(n.into(), serde_json::json!({"type": "string"}));
    }
    let schemas = schema_map(&[(
        "mini",
        serde_json::json!({"mini.v4.1.config.Thing": {"type": "object", "properties": props}}),
    )]);
    let col = |header: &str, path: &str, kind: &str| xtask::catalog::model::ColumnModel {
        header: header.into(),
        path: path.into(),
        kind: kind.into(),
    };
    let with_status_at = |i: usize| {
        let mut k = checkable(
            "mini.config.Thing",
            "mini.v4.1.config.Thing",
            "/mini/v4.1/config/things",
        );
        let names = ["a", "b", "c", "d", "e", "f", "g"];
        k.columns = names
            .iter()
            .enumerate()
            .map(|(n, p)| col("H", p, if n == i { "Status" } else { "Text" }))
            .collect();
        xtask::catalog::overlay::check(&[k], &schemas)
    };
    for i in 0..nutsh_catalog::DEFAULT_COLUMNS {
        with_status_at(i).unwrap_or_else(|e| panic!("index {i} should pass: {e}"));
    }
    let e = with_status_at(6).unwrap_err();
    assert!(
        e.to_string().contains("must be below DEFAULT_COLUMNS"),
        "{e}"
    );
}

/// A kind whose schema the generator could not infer can name it, without the version segment,
/// so a spec bump from `microseg.v4.3.config.…` to `v4.4` does not rot `curated.toml`.
#[test]
fn a_schema_override_resolves_without_its_version_segment() {
    use xtask::catalog::overlay;
    let schemas = schema_map(&[(
        "mini",
        serde_json::json!({
            "mini.v4.1.config.RealThing": {
                "type": "object",
                "properties": {"name": {"type": "string"}, "state": {"type": "string", "enum": ["UP"]}}
            }
        }),
    )]);
    let mut kinds = vec![checkable(
        "mini.config.things",
        "",
        "/mini/v4.1/config/things",
    )];
    kinds[0].fallback_columns = vec![
        xtask::catalog::model::ColumnModel {
            header: "NAME".into(),
            path: "name".into(),
            kind: "Text".into(),
        },
        xtask::catalog::model::ColumnModel {
            header: "EXT ID".into(),
            path: "extId".into(),
            kind: "Reference".into(),
        },
    ];
    let o = overlay::parse("[kinds.\"mini.config.things\"]\nschema = \"mini.config.RealThing\"\n")
        .unwrap();
    overlay::resolve_schemas(&o, &mut kinds, &schemas).unwrap();
    assert_eq!(kinds[0].schema, "mini.v4.1.config.RealThing");
    // And the fallback columns are recomputed from it, so the kind stops showing the stub.
    let cols: Vec<(&str, &str)> = kinds[0]
        .fallback_columns
        .iter()
        .map(|c| (c.path.as_str(), c.kind.as_str()))
        .collect();
    assert_eq!(cols, vec![("name", "Text"), ("state", "Status")]);

    // A name that matches nothing fails generation rather than silently doing nothing.
    let bad =
        overlay::parse("[kinds.\"mini.config.things\"]\nschema = \"mini.config.Nope\"\n").unwrap();
    let e = overlay::resolve_schemas(&bad, &mut kinds, &schemas).unwrap_err();
    assert!(e.to_string().contains("matches no schema"), "{e}");
}

#[test]
fn headers_split_camel_case() {
    use xtask::catalog::parse::header_for;
    assert_eq!(header_for("powerState"), "POWER STATE");
    assert_eq!(header_for("extId"), "EXT ID");
    assert_eq!(header_for("name"), "NAME");
    assert_eq!(header_for("ipv4Config"), "IPV4 CONFIG");
    assert_eq!(header_for("minimumAHVVersion"), "MINIMUM AHV VERSION");
    assert_eq!(header_for("serviceVMId"), "SERVICE VM ID");
    assert_eq!(header_for("isVtpmEnabled"), "IS VTPM ENABLED");
    assert_eq!(header_for("isLiveMigrateVMs"), "IS LIVE MIGRATE VMS");
    assert_eq!(header_for("numVMsTotal"), "NUM VMS TOTAL");
}

#[test]
fn overlay_applies_and_rejects_unknown_ids() {
    use xtask::catalog::overlay;
    let mut kinds = mini();
    let o = overlay::parse(
        r#"
[kinds."mini.config.Widget"]
display = "Widgets!"
aliases = ["w"]
category = "Toys"
poll_secs = 5

[[kinds."mini.config.Widget".columns]]
header = "NAME"
path = "name"
kind = "Text"
"#,
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    let w = find(&kinds, "mini.config.Widget");
    assert_eq!(w.display, "Widgets!");
    assert_eq!(w.aliases, vec!["w", "widgets", "widget"]);
    assert_eq!(w.category, "Toys");
    assert_eq!(w.poll_secs, 5);
    assert_eq!(w.columns.len(), 1);
    assert_eq!(w.columns[0].path, "name");
    assert!(w.curated);
    assert!(!find(&kinds, "mini.config.Gear").curated);

    let bad = overlay::parse("[kinds.\"nope.Kind\"]\ndisplay = \"x\"\n").unwrap();
    let err = overlay::apply(&bad, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("nope.Kind"), "{err}");

    let typo = overlay::parse("[kinds.\"mini.config.Widget\"]\npoll_sec = 5\n");
    assert!(typo.is_err(), "unknown TOML keys must be rejected");
    // Including near-misses of the newest keys: `KindOverlay` is `deny_unknown_fields`, so a
    // curated budget or order that is spelled wrong fails generation rather than being read
    // as nothing at all.
    for key in [
        "max_row = 500",
        "maxRows = 500",
        "order_by = \"a desc\"",
        "orderBy = \"a\"",
    ] {
        assert!(
            overlay::parse(&format!("[kinds.\"mini.config.Widget\"]\n{key}\n")).is_err(),
            "{key} must be rejected"
        );
    }
    let bad_kind = overlay::parse(
        "[kinds.\"mini.config.Widget\"]\ncolumns = [{ header = \"X\", path = \"x\", kind = \"Nope\" }]\n",
    )
    .unwrap();
    let err = overlay::apply(&bad_kind, &mut kinds)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Nope"), "{err}");
}

#[test]
fn overlay_curates_a_row_budget_and_an_order() {
    use xtask::catalog::overlay;
    let mut kinds = mini();
    // The mini spec declares no `$orderby`; the real Task and Alert endpoints do.
    find_mut(&mut kinds, "mini.config.Widget")
        .list_params
        .orderby = true;
    let o = overlay::parse(
        r#"
[kinds."mini.config.Widget"]
orderby = "createdTime desc"
max_rows = 500
"#,
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    let w = find(&kinds, "mini.config.Widget");
    assert_eq!(w.orderby.as_deref(), Some("createdTime desc"));
    assert_eq!(w.max_rows, Some(500));
    // Nothing is derived: a kind the overlay says nothing about walks whole, unordered.
    let g = find(&kinds, "mini.config.Gear");
    assert_eq!((g.orderby.as_deref(), g.max_rows), (None, None));

    // A budget of no rows is a typo, not a table.
    let mut kinds = mini();
    let zero = overlay::parse("[kinds.\"mini.config.Widget\"]\nmax_rows = 0\n").unwrap();
    let err = overlay::apply(&zero, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("max_rows is zero"), "{err}");

    // An order on an endpoint that takes no `$orderby` would be dropped by the client.
    let mut kinds = mini();
    let unordered =
        overlay::parse("[kinds.\"mini.config.Widget\"]\norderby = \"name asc\"\n").unwrap();
    let err = overlay::apply(&unordered, &mut kinds)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no $orderby"), "{err}");
}

/// A kind whose list endpoint takes `$orderby` and whose schema declares two timestamps: what
/// a curated `probe_by` is checked against.
fn timestamped_kind() -> KindModel {
    let mut k = checkable(
        "mini.config.Widget",
        "mini.v4.1.config.Widget",
        "/mini/v4.1/config/widgets",
    );
    k.list_params.orderby = true;
    k.timestamps = vec!["createdTime".into(), "lastUpdatedTime".into()];
    k
}

/// `overlay::apply` over a one-kind overlay curating `probe_by` and nothing else.
fn apply_probe(kind: &mut KindModel, field: &str) -> anyhow::Result<()> {
    let o =
        xtask::catalog::overlay::parse(&format!("[kinds.{:?}]\nprobe_by = {field:?}\n", kind.id))?;
    xtask::catalog::overlay::apply(&o, std::slice::from_mut(kind))
}

/// The generator refuses a `probe_by` the kind's schema does not declare as a timestamp, and
/// one on a kind whose list endpoint takes no `$orderby`. Both are curation mistakes that
/// would otherwise cost one wasted request per cycle for ever.
#[test]
fn a_probe_field_must_be_a_declared_timestamp_on_an_orderable_endpoint() {
    let mut kind = timestamped_kind();
    let mut good = kind.clone();
    apply_probe(&mut good, "lastUpdatedTime").expect("a declared timestamp is curated");
    assert_eq!(good.probe_by.as_deref(), Some("lastUpdatedTime"));
    let e = apply_probe(&mut kind.clone(), "name")
        .unwrap_err()
        .to_string();
    assert!(e.contains("not a timestamp"), "{e}");
    kind.list_params.orderby = false;
    let e = apply_probe(&mut kind, "lastUpdatedTime")
        .unwrap_err()
        .to_string();
    assert!(e.contains("$orderby"), "{e}");
}

#[test]
fn render_emits_rust_source() {
    use xtask::catalog::{model::NamespaceModel, render};
    let kinds = mini();
    let ns = vec![NamespaceModel {
        name: "mini".into(),
        version: "v4.1".into(),
        preview: false,
        versions: vec!["v4.1".into(), "v4.0".into()],
    }];
    let src = render::render(&ns, &kinds, &[], 0, &no_pages(), 0).unwrap();
    assert!(src.starts_with("// @generated"));
    assert!(src.contains("pub static NAMESPACES: &[Namespace] = &["));
    assert!(src.contains(
        "Namespace { name: \"mini\", version: \"v4.1\", preview: false, versions: &[\"v4.1\", \"v4.0\"] },"
    ));
    assert!(src.contains("        since: \"v4.1\","), "{src}");
    assert!(src.contains("pub static KINDS: &[Kind] = &["));
    assert!(src.contains("id: \"mini.config.Widget\","));
    assert!(src.contains("parent: Some(\"mini.config.Widget\"),"));
    assert!(
        src.contains(
            "Action { name: \"spin\", path: \"/mini/v4.1/config/widgets/{extId}/$actions/spin\", method: Method::Post, needs_etag: true, needs_body: false, takes_body: false, returns: ActionReturn::None, scope: &[\"extId\"], roles: &[\"Administrator\"], key: \"\", label: \"\", danger: Danger::None, confirm: ConfirmKind::None, order: 0, hidden: false, body: None, form: &[], rate: RateLimit { count: 5, per_secs: 1 } },"
        ),
        "{src}"
    );
    assert!(src.contains(
        "Column { header: \"CLUSTER\", path: \"cluster.extId\", kind: ColumnKind::Reference },"
    ));
    assert!(src.contains("list_params: ListParams { page: true, limit: true, filter: true, orderby: false, select: false, expand: false, required: false },"));
    // An uncurated kind carries neither an order, nor a probe field, nor a budget.
    assert!(
        src.contains("        orderby: None,\n        probe_by: None,\n        max_rows: None,\n"),
        "{src}"
    );
}

/// The `Field` syntax is pinned on a hand-built form rather than a derived one: every
/// `FieldKind` variant, a `Json` seed with embedded quotes, a non-ASCII label, and the two
/// names the import line gains, in alphabetical position.
#[test]
fn render_emits_form_fields_and_imports_their_types() {
    use xtask::catalog::{
        model::{FieldKind, FieldModel, NamespaceModel},
        render,
    };
    let field = |name: &str, label: &str, ty: FieldKind, value: Option<&str>| FieldModel {
        name: name.to_string(),
        label: label.to_string(),
        ty,
        required: false,
        value: value.map(str::to_string),
        hidden: false,
    };
    let mut kinds = mini();
    let widget = kinds
        .iter_mut()
        .find(|k| k.id == "mini.config.Widget")
        .unwrap();
    let spin = widget
        .actions
        .iter_mut()
        .find(|a| a.name == "spin")
        .unwrap();
    spin.form = vec![
        FieldModel {
            required: true,
            ..field("name", "Name", FieldKind::Text, None)
        },
        field("isPoweredOn", "Powered on", FieldKind::Bool, Some("true")),
        field("memoryBytes", "Mémoire", FieldKind::Integer, None),
        field(
            "mode",
            "Mode",
            FieldKind::Enum(vec!["a".into(), "b".into()]),
            None,
        ),
        field(
            "rack.extId",
            "Rack",
            FieldKind::Reference(Some("mini.config.Widget".into())),
            None,
        ),
        FieldModel {
            hidden: true,
            ..field(
                "owner.extId",
                "Owner",
                FieldKind::Reference(None),
                Some("$ext_id"),
            )
        },
        field(
            "disks",
            "Disks",
            FieldKind::Json("[{\"sizeBytes\": 0}]".into()),
            None,
        ),
    ];
    let ns = vec![NamespaceModel {
        name: "mini".into(),
        version: "v4.1".into(),
        preview: false,
        versions: vec!["v4.1".into()],
    }];
    let src = render::render(&ns, &kinds, &[], 0, &no_pages(), 0).unwrap();
    assert!(
        src.contains(
            "use crate::{Action, ActionReturn, Column, ColumnKind, ConfirmKind, Danger, Field, FieldType, Kind, ListParams, Method, Namespace, NavGroup, PageDef, RateLimit, Role};"
        ),
        "{src}"
    );
    let expected = concat!(
        "form: &[",
        r#" Field { name: "name", label: "Name", ty: FieldType::Text, required: true, value: None, hidden: false },"#,
        r#" Field { name: "isPoweredOn", label: "Powered on", ty: FieldType::Bool, required: false, value: Some("true"), hidden: false },"#,
        r#" Field { name: "memoryBytes", label: "Mémoire", ty: FieldType::Integer, required: false, value: None, hidden: false },"#,
        r#" Field { name: "mode", label: "Mode", ty: FieldType::Enum(&["a", "b"]), required: false, value: None, hidden: false },"#,
        r#" Field { name: "rack.extId", label: "Rack", ty: FieldType::Reference(Some("mini.config.Widget")), required: false, value: None, hidden: false },"#,
        r#" Field { name: "owner.extId", label: "Owner", ty: FieldType::Reference(None), required: false, value: Some("$ext_id"), hidden: true },"#,
        r#" Field { name: "disks", label: "Disks", ty: FieldType::Json("[{\"sizeBytes\": 0}]"), required: false, value: None, hidden: false },"#,
        "], rate: RateLimit { count: 5, per_secs: 1 } },",
    );
    assert!(src.contains(expected), "{src}");
}

#[test]
fn render_rejects_unknown_column_kind() {
    use xtask::catalog::{model::NamespaceModel, render};
    let mut kinds = mini();
    kinds[0].fallback_columns[0].kind = "Nope".into();
    let ns = vec![NamespaceModel {
        name: "mini".into(),
        version: "v4.1".into(),
        preview: false,
        versions: vec!["v4.1".into(), "v4.0".into()],
    }];
    assert!(render::render(&ns, &kinds, &[], 0, &no_pages(), 0).is_err());
}

/// Copy `from` into `to` recursively, creating `to`. Real files, not symlinks: the generator
/// must read a tree it could have been handed anywhere.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A checkout that lost `nav.toml` or `pages.toml` is told which file it lost. Without the
/// path the failure is a bare `No such file or directory (os error 2)`.
#[test]
fn a_lost_nav_or_pages_file_names_itself() {
    for lost in ["nav.toml", "pages.toml"] {
        let root = tempfile::tempdir().unwrap();
        copy_tree(&fixtures(), &root.path().join("specs"));
        std::fs::create_dir_all(root.path().join("crates/catalog/src")).unwrap();
        std::fs::write(root.path().join("crates/catalog/curated.toml"), "").unwrap();
        for name in ["nav.toml", "pages.toml"] {
            if name != lost {
                std::fs::write(root.path().join("crates/catalog").join(name), "").unwrap();
            }
        }
        let err = format!("{:#}", xtask::catalog::generate(root.path()).unwrap_err());
        assert!(err.contains(lost), "{err}");
        assert!(err.contains("reading"), "{err}");
    }
}

/// The committed `generated.rs` is checked for drift in CI, so two runs over the same specs
/// must agree byte for byte: no map iteration order and no timestamp may reach the output.
#[test]
fn generate_is_deterministic() {
    let root = tempfile::tempdir().unwrap();
    copy_tree(&fixtures(), &root.path().join("specs"));
    std::fs::create_dir_all(root.path().join("crates/catalog/src")).unwrap();
    std::fs::write(root.path().join("crates/catalog/curated.toml"), "").unwrap();
    // Empty, not absent: an absent file is an error naming its path, which
    // `a_lost_nav_or_pages_file_names_itself` covers; empty is what "no menu at all" looks
    // like, and it must still generate.
    std::fs::write(root.path().join("crates/catalog/nav.toml"), "").unwrap();
    std::fs::write(root.path().join("crates/catalog/pages.toml"), "").unwrap();
    let out = root.path().join("crates/catalog/src/generated.rs");

    xtask::catalog::generate(root.path()).unwrap();
    let first = std::fs::read(&out).unwrap();
    xtask::catalog::generate(root.path()).unwrap();
    let second = std::fs::read(&out).unwrap();

    assert!(!first.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&first),
        String::from_utf8_lossy(&second),
        "two runs over the same specs produced different source"
    );
}

#[test]
fn orphan_sub_resource_is_marked_unlistable() {
    let kinds = mini();
    let o = kinds
        .iter()
        .find(|k| k.list_path == "/mini/v4.1/config/orphan-parts/{orphanExtId}/bits")
        .unwrap();
    assert_eq!(o.parent, None);
    assert!(o.list_params.required);
    assert_eq!(o.id, "mini.config.Gear~bits");
}

#[test]
fn display_names_restore_acronyms() {
    use xtask::catalog::parse::prettify;
    assert_eq!(
        prettify("Vm Anti Affinity Policies"),
        "VM Anti Affinity Policies"
    );
    assert_eq!(
        prettify("Iscsi Client Attachments"),
        "iSCSI Client Attachments"
    );
    assert_eq!(prettify("Vms"), "VMs");
    assert_eq!(prettify("Widgets"), "Widgets");
}

#[test]
fn redaction_hides_secrets_and_masks_addresses_deterministically() {
    use serde_json::json;
    use xtask::record::redact;
    let v = json!({
        "extId": "abc",
        "name": "prod-db-01",
        "$reserved": {"x": 1},
        "password": "hunter2",
        "cluster": {"extId": "c1", "name": "lab"},
        "nics": [{"backingInfo": {"macAddress": "50:6b:8d:00:00:01"}, "ip": "10.0.0.9"}]
    });
    let r = redact(v.clone());
    assert_eq!(r["extId"], "abc");
    assert!(r.get("$reserved").is_none());
    assert_eq!(r["password"], "[redacted]");
    assert!(
        r["name"].as_str().unwrap().starts_with("name-"),
        "{}",
        r["name"]
    );
    assert_ne!(r["name"], "prod-db-01");
    assert_eq!(
        redact(v)["name"],
        r["name"],
        "hashing must be deterministic"
    );
    assert_eq!(
        r["nics"][0]["backingInfo"]["macAddress"],
        "00:00:5e:00:53:01"
    );
    let ip = r["nics"][0]["ip"].as_str().unwrap();
    assert!(
        ip.starts_with("203.0.113.") || ip.starts_with("198.51.100."),
        "{ip}"
    );
}

#[test]
fn redaction_covers_identity_fields_and_embedded_addresses() {
    use serde_json::json;
    use xtask::record::{leaks, redact};
    let v = json!({
        "$objectType": "clustermgmt.v4.r0.b2.config.Host",
        "hostName": "ahv-node-1.corp.example.com",
        "nodeSerial": "ABC123",
        "hypervisor": { "fullName": "Nutanix 20230302", "userName": "root" },
        "network": { "fqdn": { "value": "pc.corp.example.com" }, "ipv6": { "value": "fe80::1" } },
        "message": "VM web-01 at 10.1.2.3 (mac 50:6b:8d:00:00:01) owned by ops@corp.example.com",
        "licenseKey": "XXXX-YYYY",
        "key": "Environment"
    });
    let r = redact(v);
    assert_eq!(r["$objectType"], "clustermgmt.v4.r0.b2.config.Host");
    assert!(r["hostName"].as_str().unwrap().starts_with("hostName-"));
    assert!(r["nodeSerial"].as_str().unwrap().starts_with("nodeSerial-"));
    assert!(
        r["hypervisor"]["fullName"]
            .as_str()
            .unwrap()
            .starts_with("fullName-")
    );
    assert!(
        r["hypervisor"]["userName"]
            .as_str()
            .unwrap()
            .starts_with("userName-")
    );
    assert!(
        r["network"]["fqdn"]["value"]
            .as_str()
            .unwrap()
            .starts_with("value-")
    );
    assert!(
        r["network"]["ipv6"]["value"]
            .as_str()
            .unwrap()
            .starts_with("2001:db8:")
    );
    let msg = r["message"].as_str().unwrap();
    assert!(
        msg.contains("203.0.113.1")
            && msg.contains("00:00:5e:00:53:01")
            && msg.contains("user@example.invalid"),
        "{msg}"
    );
    assert_eq!(r["licenseKey"], "[redacted]");
    assert_eq!(r["key"], "Environment");
    assert!(
        leaks(&r, "pc.corp.example.com").is_empty(),
        "{:?}",
        leaks(&r, "pc.corp.example.com")
    );
    assert!(!leaks(&json!({"addr": "10.0.0.9", "mail": "admin@corp.com"}), "x").is_empty());
}

#[test]
fn redaction_masks_addresses_glued_to_words_and_the_host_name() {
    use serde_json::json;
    use xtask::record::{leaks, redact_from};
    // What the recording's alerts carry: the PC's own name, `PC_<ip>`, inside free text.
    let v = json!({
        "parameters": [{"paramValue": {"stringValue": "PC_10.0.0.8 unreachable"}}],
        "message": "Cannot reach PC_pc.corp.example.com from node 10.0.0.9"
    });
    let r = redact_from(v, "pc.corp.example.com");
    let text = r["parameters"][0]["paramValue"]["stringValue"]
        .as_str()
        .unwrap();
    // The bag is hashed whole, glued address and all: see `xtask/tests/record.rs`, which
    // covers what it held in the recording and why no key name can sort one value from another.
    assert!(text.starts_with("stringValue-"), "{text}");
    let msg = r["message"].as_str().unwrap();
    assert!(msg.starts_with("Cannot reach PC_host-"), "{msg}");
    assert!(msg.ends_with("from node 203.0.113.1"), "{msg}");
    assert!(
        leaks(&r, "pc.corp.example.com").is_empty(),
        "{:?}",
        leaks(&r, "pc.corp.example.com")
    );
    assert_eq!(
        leaks(&json!({"m": "PC_10.0.0.8"}), "10.0.0.8").len(),
        2,
        "the scan must see a glued address as both an address and the host"
    );
}

#[test]
fn redaction_hides_scripts_and_key_material_but_not_versions() {
    use serde_json::json;
    use xtask::record::redact;
    let v = json!({
        "guestCustomization": { "config": { "cloudInitScript": "I2Nsb3VkLWNvbmZpZwo=" } },
        "config": { "authorizedPublicKeyList": [ { "name": "ops", "key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample ops@lab" } ] },
        "categories": [ { "key": "Environment", "value": "prod" } ],
        "fullVersion": "6.5.3.5",
        "buildInfo": { "version": "7.3.0.1" }
    });
    let r = redact(v);
    assert_eq!(r["guestCustomization"], "[redacted]");
    assert_eq!(
        r["config"]["authorizedPublicKeyList"][0]["key"],
        "[redacted]"
    );
    assert_eq!(r["categories"][0]["key"], "Environment");
    assert_eq!(r["fullVersion"], "6.5.3.5");
    assert_eq!(r["buildInfo"]["version"], "7.3.0.1");
    use xtask::record::leaks;
    assert!(leaks(&r, "x").is_empty(), "{:?}", leaks(&r, "x"));
    assert!(leaks(&json!({"ip": "6.5.3.5"}), "x").len() == 1);
}

#[test]
fn redaction_hashes_hostname_carrying_keys() {
    use serde_json::json;
    use xtask::record::redact;
    let r = redact(json!({
        "smtpServer": "mail.corp.example.com",
        "endpointUrl": "https://pc.corp.example.com/api"
    }));
    assert!(
        r["smtpServer"].as_str().unwrap().starts_with("smtpServer-"),
        "{}",
        r["smtpServer"]
    );
    assert!(
        r["endpointUrl"]
            .as_str()
            .unwrap()
            .starts_with("endpointUrl-"),
        "{}",
        r["endpointUrl"]
    );
}

#[test]
fn nav_refuses_an_unknown_kind_an_unknown_page_and_an_unopenable_kind() {
    use xtask::catalog::{nav, pages};
    let kinds = mini();
    let pages_file = pages::parse(
        "[[page]]\nid = \"p\"\ntitle = \"P\"\nrow_heights = [0]\n\n[[page.pane]]\ntitle = \"W\"\nkind = \"mini.config.Widget\"\nrow = 0\ncol = 0\nweight = 1\nempty = \"none\"\n",
    )
    .unwrap();
    let ok = nav::parse(
        "[[group]]\nname = \"G\"\nitems = [{ label = \"W\", kind = \"mini.config.Widget\" }, { label = \"P\", page = \"p\" }, { label = \"X\", note = \"not in the v4 API\" }]\n",
    )
    .unwrap();
    nav::validate(&ok, &kinds, &pages_file).unwrap();

    let unknown_kind =
        nav::parse("[[group]]\nname = \"G\"\nitems = [{ label = \"N\", kind = \"nope.Kind\" }]\n")
            .unwrap();
    let err = nav::validate(&unknown_kind, &kinds, &pages_file)
        .unwrap_err()
        .to_string();
    assert!(err.contains("nope.Kind"), "{err}");

    let unknown_page =
        nav::parse("[[group]]\nname = \"G\"\nitems = [{ label = \"N\", page = \"nope\" }]\n")
            .unwrap();
    let err = nav::validate(&unknown_page, &kinds, &pages_file)
        .unwrap_err()
        .to_string();
    assert!(err.contains("nope"), "{err}");

    // `mini.config.Gear` is a sub-resource of Widget: reachable by drilling, not from the menu.
    let child = nav::parse(
        "[[group]]\nname = \"G\"\nitems = [{ label = \"G\", kind = \"mini.config.Gear\" }]\n",
    )
    .unwrap();
    let err = nav::validate(&child, &kinds, &pages_file)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("mini.config.Gear") && err.contains("Direct"),
        "{err}"
    );

    // "exactly one of" is the rule the arms enforce, not only the message they print: a note
    // beside a kind would be generated onto an item the sidebar draws no reason for.
    for source in [
        "[[group]]\nname = \"G\"\nitems = [{ label = \"W\", kind = \"mini.config.Widget\", note = \"why\" }]\n",
        "[[group]]\nname = \"G\"\nitems = [{ label = \"C\", contexts = true, note = \"why\" }]\n",
    ] {
        let stray = nav::parse(source).unwrap();
        let err = nav::validate(&stray, &kinds, &pages_file)
            .unwrap_err()
            .to_string();
        assert!(err.contains("exactly one of"), "{err}");
    }
}

/// One group per namespace is appended, holding every directly openable kind of it, sorted by
/// display name - so the invariant "everything is reachable" is a generator fact, not a UI
/// special case.
#[test]
fn nav_appends_one_group_per_namespace() {
    use xtask::catalog::nav;
    let kinds = mini();
    let tree = nav::parse("[[group]]\nname = \"G\"\nitems = []\n").unwrap();
    let groups = nav::with_namespace_groups(tree, &kinds);
    let mini_group = groups
        .iter()
        .find(|g| g.name == "mini")
        .expect("a group named after the namespace");
    let labels: Vec<&str> = mini_group.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Sprockets", "Widgets"], "sorted by display name");
    assert!(
        !mini_group.items.iter().any(|i| i.label == "Gears"),
        "sub-resources are not top-level"
    );
}

/// A page whose pane names a kind nothing has, or whose grid points off the end or repeats a
/// cell, or which asks for no width, or whose column path is not a path, fails generation -
/// every rule `pages.toml`'s own header promises.
#[test]
fn pages_are_validated_against_the_kinds_and_the_grid() {
    use xtask::catalog::pages;
    let kinds = mini();
    let good = pages::parse(&good_source()).unwrap();
    pages::validate(&good, &kinds).unwrap();

    for (from, to, needle) in [
        (
            "kind = \"mini.config.Widget\"",
            "kind = \"nope.Kind\"",
            "nope.Kind",
        ),
        ("row = 0", "row = 7", "row 7"),
        ("col = 0", "col = 2", "col 2"),
        ("weight = 1", "weight = 0", "weight 0"),
    ] {
        let bad = pages::parse(&good_source().replace(from, to)).unwrap();
        let err = pages::validate(&bad, &kinds).unwrap_err().to_string();
        assert!(err.contains(needle), "{err}");
    }

    // Two panes in one cell would draw over each other.
    let clash = pages::parse(&format!(
        "{}\n[[page.pane]]\ntitle = \"W2\"\nkind = \"mini.config.Widget\"\nrow = 0\ncol = 0\nweight = 1\nempty = \"none\"\n",
        good_source()
    ))
    .unwrap();
    let err = pages::validate(&clash, &kinds).unwrap_err().to_string();
    assert!(err.contains("repeats cell (0, 0)"), "{err}");

    let bad_path =
        pages::parse(&good_source().replace("path = \"name\"", "path = \"a..b\"")).unwrap();
    let err = pages::validate(&bad_path, &kinds).unwrap_err().to_string();
    assert!(err.contains("a..b"), "{err}");
}

/// The source `pages_are_validated_against_the_kinds_and_the_grid` mutates.
fn good_source() -> String {
    "[[page]]\nid = \"p\"\ntitle = \"P\"\nrow_heights = [0]\n\n[[page.pane]]\ntitle = \"W\"\nkind = \"mini.config.Widget\"\nrow = 0\ncol = 0\nweight = 1\nempty = \"none\"\ncolumns = [{ header = \"N\", path = \"name\", kind = \"Text\" }]\n".to_string()
}

/// A pane's columns must be a subsequence, in order, of the kind's curated columns - path and
/// kind both - unless the pane says in so many words why not. The two curation sites are not
/// redundant (the Dashboard's Alerts pane takes four of the Alert's columns and renames two of
/// them, `MESSAGE` for `TITLE` and `AGE` for `CREATED`, which is why this compares paths and
/// kinds and ignores headers), but they must not drift silently.
///
/// A kind with no curated columns constrains nothing: there is no list for the pane to be a
/// subsequence of, and the pane's own columns are then the only curation that kind has. A note
/// is checked in both directions: a blank one exempts nothing, and one on a pane that agrees
/// with its kind is reported so the exemption cannot outlive the difference it excused.
#[test]
fn a_panes_columns_are_a_subsequence_of_the_kinds() {
    use xtask::catalog::{model::ColumnModel, pages};
    let col = |header: &str, path: &str, kind: &str| ColumnModel {
        header: header.into(),
        path: path.into(),
        kind: kind.into(),
    };
    let mut thing = checkable(
        "mini.config.Thing",
        "mini.v4.1.config.Thing",
        "/mini/v4.1/config/things",
    );
    thing.columns = vec![
        col("NAME", "name", "Text"),
        col("STATE", "state", "Status"),
        col("SIZE", "sizeBytes", "Bytes"),
        col("CREATED", "createTime", "Timestamp"),
    ];
    let page = |columns: &str, note: &str| {
        format!(
            "[[page]]\nid = \"p\"\ntitle = \"P\"\nrow_heights = [0]\n\n  [[page.pane]]\n  \
             title = \"T\"\n  kind = \"mini.config.Thing\"\n  row = 0\n  col = 0\n  weight = 1\n  \
             empty = \"none\"\n{note}  columns = [{columns}]\n"
        )
    };
    // The kind is passed rather than captured: the last case clears its columns, and a closure
    // holding a borrow of it would not let that happen.
    let check = |text: String, thing: &KindModel| {
        let p = pages::parse(&text).unwrap();
        pages::validate(&p, std::slice::from_ref(thing))
    };

    // A prefix, and a gap in the middle: both are subsequences.
    check(
        page(
            "{ header = \"NAME\", path = \"name\", kind = \"Text\" }, \
             { header = \"AGE\", path = \"createTime\", kind = \"Timestamp\" }",
            "",
        ),
        &thing,
    )
    .unwrap();

    // Out of order.
    let e = check(
        page(
            "{ header = \"AGE\", path = \"createTime\", kind = \"Timestamp\" }, \
             { header = \"NAME\", path = \"name\", kind = \"Text\" }",
            "",
        ),
        &thing,
    )
    .unwrap_err();
    assert!(e.to_string().contains("not a subsequence"), "{e}");

    // A path the kind does not curate.
    let e = check(
        page("{ header = \"X\", path = \"nope\", kind = \"Text\" }", ""),
        &thing,
    )
    .unwrap_err();
    assert!(e.to_string().contains("not a subsequence"), "{e}");

    // The same path with a different kind: `nodes.numberOfNodes` as a `Count` beside the same
    // property as `Text` is the real bug this catches, and `Count` over an integer always
    // renders `1`.
    let e = check(
        page(
            "{ header = \"SIZE\", path = \"sizeBytes\", kind = \"Count\" }",
            "",
        ),
        &thing,
    )
    .unwrap_err();
    assert!(e.to_string().contains("not a subsequence"), "{e}");

    // And a pane that says why not is allowed to differ.
    check(
        page(
            "{ header = \"X\", path = \"nope\", kind = \"Text\" }",
            "  columns_note = \"the Dashboard asks a different question\"\n",
        ),
        &thing,
    )
    .unwrap();

    // A note that says nothing exempts nothing: the pane is still reported, and so is the note.
    let e = check(
        page(
            "{ header = \"X\", path = \"nope\", kind = \"Text\" }",
            "  columns_note = \"   \"\n",
        ),
        &thing,
    )
    .unwrap_err();
    assert!(e.to_string().contains("columns_note is blank"), "{e}");
    assert!(e.to_string().contains("not a subsequence"), "{e}");

    // And a note on a pane that agrees with its kind is dead weight: the check is off for that
    // pane while nothing is being excused, which is how an exemption becomes permanent.
    let e = check(
        page(
            "{ header = \"NAME\", path = \"name\", kind = \"Text\" }",
            "  columns_note = \"the Dashboard asks a different question\"\n",
        ),
        &thing,
    )
    .unwrap_err();
    assert!(e.to_string().contains("no longer needed"), "{e}");

    // An uncurated kind constrains nothing.
    thing.columns.clear();
    check(
        page("{ header = \"X\", path = \"nope\", kind = \"Text\" }", ""),
        &thing,
    )
    .unwrap();
}

/// The generator's own `Reach::Direct` predicate must agree with the catalog's, which reads
/// the committed `generated.rs` and is therefore the *previous* catalog during a regeneration.
#[test]
fn the_generators_direct_predicate_agrees_with_the_catalogs() {
    use xtask::catalog::model::ListParamsModel;
    use xtask::catalog::nav::is_direct;
    for k in nutsh_catalog::KINDS {
        let model = KindModel {
            id: k.id.to_string(),
            list_path: k.list_path.to_string(),
            parent: k.parent.map(str::to_string),
            list_params: ListParamsModel {
                required: k.list_params.required,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            is_direct(&model),
            nutsh_catalog::reach(k) == nutsh_catalog::Reach::Direct,
            "{}",
            k.id
        );
    }
}

#[test]
fn record_refuses_the_curated_fixture_directory() {
    use xtask::record::{RecordArgs, record};
    let curated = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("crates/mockpc/fixtures");
    let args = RecordArgs {
        host: "pc.invalid".into(),
        port: 9440,
        username: "admin".into(),
        password: "x".into(),
        insecure: true,
        ca_bundle: None,
        kinds: Vec::new(),
        pages: 1,
        // The same directory spelled differently: the check resolves both before comparing.
        out: curated.join("vmm/.."),
        curated_dir: curated,
    };
    let err = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(record(args))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("refusing to record into the curated fixture directory"),
        "{err}"
    );
}

/// `warm = true` is refused on a kind nothing can list: the warm-up would have no request to
/// make, and a flag that silently does nothing is worse than one that fails generation.
#[test]
fn warm_is_refused_on_a_kind_that_cannot_be_listed() {
    let schemas = schema_map(&[(
        "mini",
        serde_json::json!({
            "mini.v4.1.config.Gear": {"type": "object", "properties": {"name": {"type": "string"}}}
        }),
    )]);
    // A sub-resource: its list path carries a placeholder its parent fills.
    let mut child = checkable(
        "mini.config.Gear",
        "mini.v4.1.config.Gear",
        "/mini/v4.1/config/widgets/{widgetExtId}/gears",
    );
    child.parent = Some("mini.config.Widget".to_string());
    child.warm = true;
    let e = xtask::catalog::overlay::check(&[child], &schemas).unwrap_err();
    assert!(
        e.to_string().contains("mini.config.Gear")
            && e.to_string()
                .contains("warm needs a kind that can be listed"),
        "{e}"
    );

    // And on one whose list needs a query parameter, for the same reason.
    let mut stats = checkable(
        "mini.stats.WidgetStats",
        "mini.v4.1.config.Gear",
        "/mini/v4.1/stats/widgets",
    );
    stats.list_params.required = true;
    stats.warm = true;
    let e = xtask::catalog::overlay::check(&[stats], &schemas).unwrap_err();
    assert!(
        e.to_string()
            .contains("warm needs a kind that can be listed"),
        "{e}"
    );
}

/// Warming a kind whose `Entity::name` falls back to its extId achieves nothing: the cache
/// would learn `id -> id` and the cell would print a 36-character UUID where it prints an
/// 8-character stub today. So the flag needs a name to cache.
#[test]
fn warm_is_refused_on_a_kind_that_names_itself_with_its_ext_id() {
    let schemas = schema_map(&[(
        "mini",
        serde_json::json!({
            "mini.v4.1.config.Nameless": {
                "type": "object",
                "properties": {"extId": {"type": "string"}, "rpm": {"type": "integer"}}
            },
            "mini.v4.1.config.Named": {
                "type": "object",
                "properties": {"extId": {"type": "string"}, "hostName": {"type": "string"}}
            },
            "mini.v4.1.config.Nested": {
                "type": "object",
                "properties": {"extId": {"type": "string"}, "config": {"type": "object"}}
            }
        }),
    )]);
    let mut nameless = checkable(
        "mini.config.Nameless",
        "mini.v4.1.config.Nameless",
        "/mini/v4.1/config/nameless",
    );
    nameless.warm = true;
    let e = xtask::catalog::overlay::check(&[nameless], &schemas).unwrap_err();
    assert!(
        e.to_string()
            .contains("warm needs name_path or a NAME_KEYS property"),
        "{e}"
    );

    // Any `NAME_KEYS` property is enough - `hostName` is how a host names itself.
    let mut named = checkable(
        "mini.config.Named",
        "mini.v4.1.config.Named",
        "/mini/v4.1/config/named",
    );
    named.warm = true;
    xtask::catalog::overlay::check(&[named], &schemas).unwrap();

    // And so is a curated `name_path`, which is the whole point of the field.
    let mut nested = checkable(
        "mini.config.Nested",
        "mini.v4.1.config.Nested",
        "/mini/v4.1/config/nested",
    );
    nested.warm = true;
    nested.name_path = "config.name".to_string();
    xtask::catalog::overlay::check(&[nested], &schemas).unwrap();
}

/// The overlay's two new keys reach the model.
#[test]
fn name_path_and_warm_come_from_the_overlay() {
    use xtask::catalog::overlay;
    let mut kinds = mini();
    let o = overlay::parse(
        "[kinds.\"mini.config.Widget\"]\nname_path = \"config.name\"\nwarm = true\n",
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    let w = find(&kinds, "mini.config.Widget");
    assert_eq!(w.name_path, "config.name");
    assert!(w.warm);
    // Untouched kinds keep the defaults, which is what `generated.rs` shows for 252 of them.
    let g = find(&kinds, "mini.config.Gear");
    assert_eq!(g.name_path, "");
    assert!(!g.warm);
}
