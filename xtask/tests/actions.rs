//! Action shape derived from an OpenAPI document: `returns`, `scope`, the two body questions,
//! and the rate tier. The `acts` fixture exercises the branches no shipped spec has all of.

use std::path::{Path, PathBuf};

use nutsh_catalog::RateLimit;
use xtask::catalog::model::{ActionModel, FieldKind, KindModel};
use xtask::catalog::spec::{self, SpecFile};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The `acts` namespace as the generator sees it before the overlay: parsed, then the
/// reference post-pass that turns a `$ref` short name into a kind id.
fn acts() -> Vec<KindModel> {
    let files = spec::select_latest(&fixtures()).unwrap();
    let f = files.iter().find(|f| f.namespace == "acts").unwrap();
    let doc = spec::load(&f.path).unwrap();
    let mut kinds = xtask::catalog::parse::parse_namespace(f, &doc).unwrap();
    xtask::catalog::parse::resolve_field_references(&mut kinds);
    kinds
}

fn find<'a>(kinds: &'a [KindModel], id: &str) -> &'a KindModel {
    kinds.iter().find(|k| k.id == id).unwrap_or_else(|| {
        panic!(
            "no kind {id} in {:?}",
            kinds.iter().map(|k| &k.id).collect::<Vec<_>>()
        )
    })
}

fn action<'a>(k: &'a KindModel, name: &str) -> &'a ActionModel {
    k.actions
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no action {name} on {}: {:?}",
                k.id,
                k.actions.iter().map(|a| &a.name).collect::<Vec<_>>()
            )
        })
}

#[test]
fn returns_is_derived_from_the_success_response() {
    let kinds = acts();
    let m = find(&kinds, "acts.config.Machine");
    // The model holds the variant name; `render` turns it into `ActionReturn::<name>`.
    assert_eq!(action(m, "start").returns, "Task", "202 $ref");
    assert_eq!(action(m, "clone").returns, "Task", "202 oneOf");
    assert_eq!(
        action(m, "quiesce").returns,
        "Payload",
        "200 AppMessage, which is what task cancel really is"
    );
    assert_eq!(action(m, "ping").returns, "None", "204 only");
    assert_eq!(action(m, "create").returns, "Task");
}

#[test]
fn needs_body_is_required_and_takes_body_is_declared() {
    let kinds = acts();
    let m = find(&kinds, "acts.config.Machine");
    let q = action(m, "quiesce");
    assert_eq!((q.takes_body, q.needs_body), (true, true), "required: true");
    let c = action(m, "clone");
    assert_eq!(
        (c.takes_body, c.needs_body),
        (true, false),
        "declared without `required`, so optional by OpenAPI default"
    );
    let s = action(m, "start");
    assert_eq!(
        (s.takes_body, s.needs_body),
        (false, false),
        "no requestBody"
    );
}

#[test]
fn scope_lists_the_placeholders_left_to_right() {
    let kinds = acts();
    let m = find(&kinds, "acts.config.Machine");
    assert_eq!(action(m, "start").scope, vec!["extId".to_string()]);
    assert_eq!(action(m, "create").scope, Vec::<String>::new());
    let n = find(&kinds, "acts.config.Node~nodes");
    assert_eq!(
        action(n, "park").scope,
        vec!["rackExtId".to_string(), "extId".to_string()]
    );
}

#[test]
fn rate_is_the_most_restrictive_tier_whatever_its_spelling() {
    let kinds = acts();
    let m = find(&kinds, "acts.config.Machine");
    // xsmall 30/s versus Large 60/minute: one a minute is the tighter budget.
    assert_eq!(
        m.rate,
        RateLimit {
            count: 60,
            per_secs: 60
        }
    );
    // `default`, a tier with no `type` at all, and `xlarge` in minutes.
    assert_eq!(
        action(m, "start").rate,
        RateLimit {
            count: 60,
            per_secs: 60
        }
    );
    assert_eq!(
        action(m, "clone").rate,
        RateLimit {
            count: 5,
            per_secs: 1
        },
        "no x-rate-limit is the default budget"
    );
}

#[test]
fn an_unknown_rate_tier_fails_generation() {
    let doc: serde_json::Value = serde_yaml_ng::from_str(
        r#"
paths:
  /x/v4.1/config/things:
    get:
      x-rate-limit:
        - type: gigantic
          count: 1
          timeUnit: seconds
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  data:
                    type: array
                    items:
                      $ref: '#/components/schemas/x.v4.1.config.Thing'
components:
  schemas:
    x.v4.1.config.Thing:
      type: object
      properties:
        extId:
          type: string
"#,
    )
    .unwrap();
    let f = SpecFile {
        namespace: "x".into(),
        version: "v4.1".into(),
        preview: false,
        path: PathBuf::from("x/v4.1.yaml"),
        versions: vec!["v4.1".into()],
    };
    let err = xtask::catalog::parse::parse_namespace(&f, &doc)
        .unwrap_err()
        .to_string();
    assert!(err.contains("gigantic"), "{err}");
    assert!(err.contains("xsmall"), "{err}");
}

/// The fixture-wide invariant `clear_unfetchable_etags` maintains: every action that keeps
/// `needs_etag` is fetchable with `get_in`, which fills exactly the get path's placeholders.
/// The clearing itself is unit-tested in `parse.rs` on a kind whose counts diverge; every
/// action the first pass attaches sits on the get path, so nothing here is ever cleared.
#[test]
fn no_kept_needs_etag_is_unfetchable() {
    for k in acts() {
        let Some(gp) = k.get_path.as_deref() else {
            assert!(
                k.actions.iter().all(|a| !a.needs_etag),
                "{}: no get path, so no action may need an ETag",
                k.id
            );
            continue;
        };
        for a in &k.actions {
            if a.needs_etag {
                assert_eq!(
                    nutsh_catalog::placeholder_count(&a.path),
                    nutsh_catalog::placeholder_count(gp),
                    "{}: {} cannot fetch its ETag",
                    k.id,
                    a.name
                );
            }
        }
    }
}

fn acts_full() -> xtask::catalog::parse::Parsed {
    let files = spec::select_latest(&fixtures()).unwrap();
    let f = files.iter().find(|f| f.namespace == "acts").unwrap();
    let doc = spec::load(&f.path).unwrap();
    xtask::catalog::parse::parse_namespace_full(f, &doc).unwrap()
}

/// The second pass claims what the first left: the prefix's trailing two segments name the
/// kind, with placeholder names normalised. Exactly one match attaches.
#[test]
fn the_second_pass_attaches_an_operations_action_to_the_nested_kind() {
    let parsed = acts_full();
    let nested = find(&parsed.kinds, "acts.config.Node~nodes");
    assert_eq!(
        action(nested, "service").path,
        "/acts/v4.1/operations/racks/{rackExtId}/nodes/{extId}/$actions/service"
    );
    assert_eq!(
        action(nested, "service").scope,
        vec!["rackExtId".to_string(), "extId".to_string()]
    );
    // The flat kind has no get path, so it cannot own the action; it borrows it.
    let flat = find(&parsed.kinds, "acts.config.Node");
    assert!(flat.get_path.is_none());
    assert!(flat.actions.is_empty());
}

/// Zero or several matches leave the action unattached, reported and never fatal: an
/// unattached action is a missing feature, not drift.
#[test]
fn an_ambiguous_operations_action_is_left_unattached() {
    let parsed = acts_full();
    let names: Vec<&str> = parsed.unattached.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["eject"]);
    assert_eq!(
        parsed.unattached[0].path,
        "/acts/v4.1/operations/bays/{bayExtId}/slots/{extId}/$actions/eject"
    );
    for k in &parsed.kinds {
        assert!(
            k.actions.iter().all(|a| a.name != "eject"),
            "{} claimed the ambiguous action",
            k.id
        );
    }
}

/// `extra_actions` claims an unattached path explicitly; an undeclared path fails generation,
/// as an unknown kind id already does.
#[test]
fn extra_actions_claims_an_unattached_path() {
    use xtask::catalog::overlay;
    let mut parsed = acts_full();
    let o = overlay::parse(
        r#"
[kinds."acts.config.Slot"]
extra_actions = ["/acts/v4.1/operations/bays/{bayExtId}/slots/{extId}/$actions/eject"]
"#,
    )
    .unwrap();
    overlay::claim_extra_actions(&o, &mut parsed.kinds, &parsed.unattached).unwrap();
    let slot = find(&parsed.kinds, "acts.config.Slot");
    assert_eq!(action(slot, "eject").method, "Post");

    let mut parsed = acts_full();
    let bad = overlay::parse(
        r#"
[kinds."acts.config.Slot"]
extra_actions = ["/acts/v4.1/operations/nowhere/{x}/$actions/nope"]
"#,
    )
    .unwrap();
    let err = overlay::claim_extra_actions(&bad, &mut parsed.kinds, &parsed.unattached)
        .unwrap_err()
        .to_string();
    assert!(err.contains("nowhere"), "{err}");
}

/// The real reason this exists: both Host kinds ship no actions today.
#[test]
fn host_maintenance_lands_on_the_cluster_scoped_host_kind() {
    let hosts = nutsh_catalog::kind("clustermgmt.config.Host~hosts").unwrap();
    let enter = hosts
        .action("enter-host-maintenance")
        .expect("attached by the second pass");
    assert!(enter.path.contains("/operations/clusters/"));
    assert_eq!(enter.scope, ["clusterExtId", "extId"]);
    assert_eq!(enter.parents_needed(), 1);
    assert!(hosts.action("exit-host-maintenance").is_some());
}

/// `RESERVED_KEYS` registers every key the program binds, navigation *and* action, so the
/// generation check refuses the navigation half only. This guards the seam rather than
/// trusting it: an exemption that widened would let an action shadow `j`.
#[test]
fn the_action_key_exemption_is_a_subset_of_the_reserved_list_and_holds_no_navigation_key() {
    use xtask::catalog::overlay::ACTION_KEYS;
    for k in ACTION_KEYS {
        assert!(
            nutsh_catalog::RESERVED_KEYS.contains(k),
            "{k:?} is exempted from a list it is not in"
        );
    }
    for k in [
        ":",
        "/",
        "j",
        "k",
        "g",
        "G",
        "ctrl-r",
        "ctrl-c",
        "space",
        "a",
        "q",
        "y",
        "S",
        "w",
        "tab",
        "shift-tab",
        "ctrl-b",
        "ctrl-e",
        "O",
        "left",
        "right",
        "enter",
        "esc",
    ] {
        assert!(
            nutsh_catalog::RESERVED_KEYS.contains(&k),
            "{k:?} is missing"
        );
        assert!(
            !ACTION_KEYS.contains(&k),
            "{k:?} is the table's, and no action may claim it"
        );
    }
    // Everything the curated set binds must be generatable.
    for k in ["p", "P", "r", "s", "m", "ctrl-d", "c", "C", "A", "R", "M"] {
        assert!(
            !nutsh_catalog::RESERVED_KEYS.contains(&k) || ACTION_KEYS.contains(&k),
            "{k:?} is reserved and not exempt, so no action could bind it"
        );
    }
}

#[test]
fn a_reserved_or_duplicated_key_fails_generation() {
    use xtask::catalog::overlay;
    let mut kinds = acts();
    let bad = overlay::parse(
        r#"
[kinds."acts.config.Machine".actions."start"]
key = "j"
"#,
    )
    .unwrap();
    let err = overlay::apply(&bad, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("\"j\""), "{err}");
    assert!(err.contains("reserved"), "{err}");

    let mut kinds = acts();
    let dup = overlay::parse(
        r#"
[kinds."acts.config.Machine".actions."start"]
key = "z"
[kinds."acts.config.Machine".actions."ping"]
key = "z"
"#,
    )
    .unwrap();
    let err = overlay::apply(&dup, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("\"z\""), "{err}");
    assert!(err.contains("two actions claim"), "{err}");
}

/// A curated label wider than the menu's narrowest box fails generation rather than being cut
/// at draw time: a label the box trims leaves no trace on the frame, so nobody finds out.
#[test]
fn an_over_wide_action_label_fails_generation() {
    use xtask::catalog::overlay;
    let mut kinds = acts();
    let long = "x".repeat(nutsh_catalog::ACTION_LABEL + 1);
    let bad = overlay::parse(&format!(
        r#"
[kinds."acts.config.Machine".actions."start"]
label = "{long}"
"#
    ))
    .unwrap();
    let err = overlay::apply(&bad, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("at most"), "{err}");

    // And the widest legal one passes, so the rule is a boundary rather than a ban.
    let mut kinds = acts();
    let fits = "x".repeat(nutsh_catalog::ACTION_LABEL);
    let ok = overlay::parse(&format!(
        r#"
[kinds."acts.config.Machine".actions."start"]
label = "{fits}"
"#
    ))
    .unwrap();
    overlay::apply(&ok, &mut kinds).expect("the widest legal label");
}

/// `c` means cancel on every table, so only an action named `cancel` may claim it.
#[test]
fn c_is_reserved_for_cancel() {
    use xtask::catalog::overlay;
    let mut kinds = acts();
    let bad = overlay::parse(
        r#"
[kinds."acts.config.Machine".actions."clone"]
key = "c"
"#,
    )
    .unwrap();
    let err = overlay::apply(&bad, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("cancel"), "{err}");

    let task = nutsh_catalog::kind("prism.config.Task").unwrap();
    assert!(
        task.action("cancel").is_some(),
        "the real cancel is what the rule protects"
    );
}

#[test]
fn field_names_follow_the_write_grammar() {
    use nutsh_catalog::valid_field_name;
    assert!(valid_field_name("name"));
    assert!(valid_field_name("spec.hostExtId"));
    assert!(valid_field_name("vmRecoveryPoints[0].vmExtId"));
    assert!(valid_field_name("a[15].b"));
    assert!(!valid_field_name("a..b"));
    assert!(!valid_field_name("a[]"));
    assert!(!valid_field_name("a[99]"));
    assert!(!valid_field_name("a[-1]"));
    assert!(!valid_field_name(""));
    assert!(!valid_field_name(".a"));
    assert!(!valid_field_name("a."));
}

/// Top-level properties only, on purpose: the whole request-schema graph would multiply
/// `generated.rs` by an order of magnitude for a form nobody can fill in a terminal anyway.
#[test]
fn form_fields_come_from_the_top_level_request_schema() {
    let kinds = acts();
    let m = find(&kinds, "acts.config.Machine");
    let clone = action(m, "clone");
    let by = |n: &str| clone.form.iter().find(|f| f.name == n).unwrap();
    assert_eq!(by("name").ty, FieldKind::Text);
    assert_eq!(by("name").label, "Name");
    assert!(!by("name").required, "CloneSpec declares no required list");
    assert_eq!(by("isPoweredOn").ty, FieldKind::Bool);
    assert_eq!(
        by("mode").ty,
        FieldKind::Enum(vec!["FAST".into(), "FULL".into()])
    );
    // `acts.config.Rack~racks` shares Rack's schema: a nested view of one listing is not an
    // ambiguity, and the picker opens over the top-level kind.
    find(&kinds, "acts.config.Rack~racks");
    assert_eq!(
        by("rack").ty,
        FieldKind::Reference(Some("acts.config.Rack".into())),
        "resolved from the $ref's short name with the Reference suffix dropped"
    );
    assert_eq!(
        by("disks").ty,
        FieldKind::Json("[{\"sizeBytes\": null}]".into()),
        "an array becomes one Json field seeded from its item's required properties"
    );
    assert_eq!(
        by("tags").ty,
        FieldKind::Json("[]".into()),
        "an array of scalars starts empty; `[{{}}]` would be a wrong body"
    );
    let create = action(m, "create");
    let names: Vec<&str> = create.form.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["name", "memoryBytes"],
        "readOnly properties are the server's, never a form's"
    );
    assert!(
        create
            .form
            .iter()
            .find(|f| f.name == "name")
            .unwrap()
            .required,
        "MachineSpec requires name"
    );
    assert_eq!(
        create
            .form
            .iter()
            .find(|f| f.name == "memoryBytes")
            .unwrap()
            .ty,
        FieldKind::Integer
    );
    assert!(
        action(m, "start").form.is_empty(),
        "no requestBody, no form"
    );
}

/// `from` is the one overlay field that creates an action rather than curating one.
#[test]
fn from_copies_an_action_within_a_kind_and_across_kinds() {
    use xtask::catalog::overlay;
    let mut kinds = acts();
    let o = overlay::parse(
        r#"
[kinds."acts.config.Machine".actions."quiesce"]
hidden = true

[kinds."acts.config.Machine".actions."freeze"]
from = "quiesce"
label = "Freeze"
key = "F"
danger = "medium"
body = { mode = "HARD" }

[kinds."acts.config.Machine".actions."service-it"]
from = "acts.config.Node~nodes:service"
label = "Service"
"#,
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    let m = find(&kinds, "acts.config.Machine");
    assert!(action(m, "quiesce").hidden);
    let f = action(m, "freeze");
    assert_eq!(f.path, action(m, "quiesce").path, "the endpoint is copied");
    assert_eq!(f.label, "Freeze");
    assert_eq!(f.key, "F");
    assert_eq!(f.danger, "Medium");
    assert_eq!(f.confirm, "Yes", "medium defaults to a yes confirm");
    assert_eq!(f.body.as_deref(), Some(r#"{"mode":"HARD"}"#));
    assert!(!f.hidden, "the copy is not the hidden original");
    let s = action(m, "service-it");
    assert_eq!(
        s.path,
        "/acts/v4.1/operations/racks/{rackExtId}/nodes/{extId}/$actions/service"
    );
    assert!(
        !s.needs_etag,
        "two placeholders against Machine's one-placeholder get path: no ETag to fetch"
    );

    let mut kinds = acts();
    let bad = overlay::parse(
        r#"
[kinds."acts.config.Machine".actions."nope"]
from = "not-an-action"
"#,
    )
    .unwrap();
    let err = overlay::apply(&bad, &mut kinds).unwrap_err().to_string();
    assert!(err.contains("not-an-action"), "{err}");
    assert_eq!(
        err.matches("not-an-action").count(),
        1,
        "reported once, by the pass that looked it up: {err}"
    );

    // A copy over an existing name would leave two actions answering to it.
    let dup =
        overlay::parse("[kinds.\"acts.config.Machine\".actions.\"clone\"]\nfrom = \"start\"\n")
            .unwrap();
    let err = overlay::apply(&dup, &mut acts()).unwrap_err().to_string();
    assert!(err.contains("would duplicate action clone"), "{err}");
}

/// A kind whose own path cannot carry an action borrows another kind's, and names the dotted
/// paths that fill the leading placeholders.
#[test]
fn actions_from_and_action_parents_and_ext_id_key() {
    use xtask::catalog::overlay;
    let mut kinds = acts();
    let o = overlay::parse(
        r#"
[kinds."acts.config.Node"]
actions_from = "acts.config.Node~nodes"
action_parents = ["rack.uuid"]

[kinds."acts.config.Slot"]
ext_id_key = "slotExtId"
"#,
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    let flat = find(&kinds, "acts.config.Node");
    assert_eq!(flat.action_kind.as_deref(), Some("acts.config.Node~nodes"));
    assert_eq!(flat.action_parents, vec!["rack.uuid".to_string()]);
    assert_eq!(find(&kinds, "acts.config.Slot").ext_id_key, "slotExtId");

    let bad = overlay::parse("[kinds.\"acts.config.Node\"]\nactions_from = \"acts.config.Nope\"\n")
        .unwrap();
    let err = overlay::apply(&bad, &mut acts()).unwrap_err().to_string();
    assert!(err.contains("acts.config.Nope"), "{err}");
}

/// Every curated mistake fails generation with a message that names it; a typo that fell
/// through to a default would be a form nobody asked for, found at runtime.
#[test]
fn each_bad_overlay_fails_generation_with_its_own_message() {
    use xtask::catalog::overlay;
    let cases: &[(&str, &str)] = &[
        (
            "[kinds.\"acts.config.Machine\".actions.\"start\"]\ndanger = \"lethal\"\n",
            "danger \"lethal\" is not one of none, low, medium, high",
        ),
        (
            "[kinds.\"acts.config.Machine\".actions.\"start\"]\nconfirm = \"maybe\"\n",
            "confirm \"maybe\" is not one of none, yes, type-name",
        ),
        (
            "[kinds.\"acts.config.Machine\".actions.\"nope\"]\nkey = \"x\"\n",
            "no action nope",
        ),
        (
            "[kinds.\"acts.config.Machine\".actions.\"clone\"]\nform = [{ name = \"a..b\" }]\n",
            "field name \"a..b\" is not seg([n])?(.seg([n])?)*",
        ),
        (
            "[kinds.\"acts.config.Machine\".actions.\"clone\"]\nform = [{ name = \"name\", type = \"strin\" }]\n",
            "field name type \"strin\" is not one of string, bool, integer, enum, reference, json",
        ),
        (
            "[kinds.\"acts.config.Machine\".actions.\"clone\"]\nform = [{ name = \"rack\", type = \"reference\", kind = \"acts.config.Nope\" }]\n",
            "reference field rack names no kind acts.config.Nope",
        ),
        (
            "[kinds.\"acts.config.Machine\".actions.\"clone\"]\nform = [{ name = \"mode\", type = \"enum\" }]\n",
            "enum field mode lists no values",
        ),
    ];
    for (text, phrase) in cases {
        let o = overlay::parse(text).unwrap();
        let err = overlay::apply(&o, &mut acts()).unwrap_err().to_string();
        assert!(
            err.contains(phrase),
            "{text}\n  expected {phrase:?}\n  got {err}"
        );
    }
}

/// `confirm` beats the floor `danger` sets; `order` and `needs_etag` are taken as written.
#[test]
fn confirm_order_and_needs_etag_overrides_are_applied() {
    use xtask::catalog::overlay;
    let mut kinds = acts();
    let o = overlay::parse(
        r#"
[kinds."acts.config.Machine".actions."update"]
danger = "high"
confirm = "yes"
order = 7
needs_etag = false

[kinds."acts.config.Machine".actions."start"]
danger = "high"

[kinds."acts.config.Machine".actions."ping"]
danger = "low"
"#,
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    let m = find(&kinds, "acts.config.Machine");
    let u = action(m, "update");
    assert_eq!(u.danger, "High");
    assert_eq!(
        u.confirm, "Yes",
        "an explicit confirm beats the TypeName floor"
    );
    assert_eq!(u.order, 7);
    assert!(
        !u.needs_etag,
        "a PUT wants an ETag unless the overlay says otherwise"
    );
    assert_eq!(
        action(m, "start").confirm,
        "TypeName",
        "high defaults to the name"
    );
    assert_eq!(action(m, "ping").confirm, "None", "low runs");
}

/// A curated form is the overlay's list, in its order; a name the schema declares keeps what
/// the generator derived unless the overlay overrides it, and a name it does not is new.
#[test]
fn a_curated_form_keeps_the_derived_field_and_appends_new_ones_in_overlay_order() {
    use xtask::catalog::overlay;
    let mut kinds = acts();
    let o = overlay::parse(
        r#"
[kinds."acts.config.Machine".actions."create"]
form = [
  { name = "name", label = "New name" },
  { name = "extra", type = "bool" },
  { name = "memoryBytes", required = true },
]
"#,
    )
    .unwrap();
    overlay::apply(&o, &mut kinds).unwrap();
    let create = action(find(&kinds, "acts.config.Machine"), "create");
    let names: Vec<&str> = create.form.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["name", "extra", "memoryBytes"]);
    let name = &create.form[0];
    assert_eq!(name.label, "New name");
    assert_eq!(name.ty, FieldKind::Text, "the derived type is kept");
    assert!(
        name.required,
        "MachineSpec requires name, and the overlay did not say otherwise"
    );
    let extra = &create.form[1];
    assert_eq!(extra.ty, FieldKind::Bool);
    assert!(
        !extra.required,
        "nothing derived, nothing curated: not required"
    );
    assert_eq!(
        extra.label, "",
        "a new field has no derived label to fall back on"
    );
    let memory = &create.form[2];
    assert_eq!(memory.ty, FieldKind::Integer, "the derived type is kept");
    assert_eq!(memory.label, "Memory bytes", "the derived label is kept");
    assert!(
        memory.required,
        "the overlay may require what the schema does not"
    );
}

/// The eleven headline kinds are what a table is worth reading for. Every path here was read
/// out of a recording or a spec; a typo shows up as a column of `-`, which no test can
/// see, so the shape is asserted instead.
#[test]
fn the_eleven_headline_kinds_have_curated_columns() {
    let expected: &[(&str, &[(&str, &str)])] = &[
        (
            "vmm.ahv.config.Vm",
            &[
                ("NAME", "name"),
                ("POWER", "powerState"),
                ("CLUSTER", "cluster.extId"),
                ("HOST", "host.extId"),
                ("IP", "nics[].nicNetworkInfo.ipv4Config.ipAddress.value"),
                ("MEM", "memorySizeBytes"),
                ("SOCKETS", "numSockets"),
                ("PROTECTION", "protectionType"),
                // Ninth, under `w`: the Dashboard's VMs pane asks for it, and a pane's
                // columns must be a subsequence of the kind's.
                ("CREATED", "createTime"),
            ],
        ),
        (
            "clustermgmt.config.Cluster",
            &[
                ("NAME", "name"),
                ("MODE", "config.operationMode"),
                ("NODES", "nodes.numberOfNodes"),
                ("VMS", "vmCount"),
                ("VERSION", "config.buildInfo.version"),
                ("EXTERNAL IP", "network.externalAddress.ipv4.value"),
                (
                    "FT",
                    "config.faultToleranceState.currentClusterFaultTolerance",
                ),
            ],
        ),
        (
            "clustermgmt.config.Host",
            &[
                ("HOST", "hostName"),
                // Inside DEFAULT_COLUMNS, and second, so the column that tints the row is
                // visible without scrolling.
                ("MAINTENANCE", "maintenanceState"),
                ("CLUSTER", "cluster.uuid"),
                ("HYPERVISOR", "hypervisor.type"),
                ("CVM IP", "controllerVm.externalAddress.ipv4.value"),
                ("HYPERVISOR IP", "hypervisor.externalAddress.ipv4.value"),
                ("VMS", "hypervisor.numberOfVms"),
                // `bootTimeUsecs` is int64 microseconds, which `ColumnKind::Micros` renders
                // as an age.
                ("UPTIME", "bootTimeUsecs"),
            ],
        ),
        (
            "prism.config.Task",
            &[
                ("OPERATION", "operationDescription"),
                ("STATUS", "status"),
                ("PROGRESS", "progressPercentage"),
                ("ENTITY", "entitiesAffected[].name"),
                ("STARTED", "startedTime"),
                ("COMPLETED", "completedTime"),
                ("CANCELABLE", "isCancelable"),
            ],
        ),
        (
            "monitoring.serviceability.Alert",
            &[
                ("SEVERITY", "severity"),
                ("STATUS", "status"),
                ("TITLE", "title"),
                ("SOURCE", "sourceEntity.name"),
                ("CLUSTER", "clusterName"),
                ("CREATED", "creationTime"),
                ("LAST", "lastOccurredTime"),
            ],
        ),
        (
            "networking.config.Subnet",
            &[
                ("NAME", "name"),
                ("TYPE", "subnetType"),
                ("VLAN", "networkId"),
                ("SUBNET", "ipConfig[].ipv4.ipSubnet.ip.value"),
                ("GATEWAY", "ipConfig[].ipv4.defaultGatewayIp.value"),
                ("CLUSTER", "clusterReference"),
                ("VPC", "vpcReference"),
            ],
        ),
        (
            "clustermgmt.config.StorageContainer",
            &[
                ("NAME", "name"),
                ("CLUSTER", "clusterName"),
                ("CAPACITY", "maxCapacityBytes"),
                ("RF", "replicationFactor"),
                ("COMPRESSION", "isCompressionEnabled"),
                ("EC", "erasureCode"),
                ("REMOVING", "isMarkedForRemoval"),
            ],
        ),
        (
            "vmm.content.Image",
            &[
                ("NAME", "name"),
                ("TYPE", "type"),
                ("SIZE", "sizeBytes"),
                ("CLUSTERS", "clusterLocationExtIds"),
                ("OWNER", "ownerName"),
                // Inside DEFAULT_COLUMNS, or it would tint nothing.
                ("COMPLIANCE", "placementPolicyStatus[].complianceStatus"),
                ("CREATED", "createTime"),
            ],
        ),
        (
            "dataprotection.config.RecoveryPoint",
            &[
                ("NAME", "name"),
                ("STATUS", "status"),
                ("TYPE", "recoveryPointType"),
                ("CREATED", "creationTime"),
                ("EXPIRES", "expirationTime"),
                ("SIZE", "totalExclusiveUsageBytes"),
                ("VMS", "vmRecoveryPoints"),
            ],
        ),
        (
            "prism.config.Category",
            &[
                ("KEY", "key"),
                ("VALUE", "value"),
                ("TYPE", "type"),
                ("DESCRIPTION", "description"),
                ("SHARED", "isSharedWithAllProjects"),
            ],
        ),
        (
            "volumes.config.VolumeGroup",
            &[
                ("NAME", "name"),
                ("CLUSTER", "clusterReference"),
                ("SHARING", "sharingStatus"),
                ("ATTACH TYPE", "attachmentType"),
                ("PROTOCOL", "protocol"),
                // Inside DEFAULT_COLUMNS, or it would tint nothing.
                ("HYDRATION", "hydrationStatus"),
                ("ATTACHMENTS", "attachments"),
                ("TARGET", "targetName"),
            ],
        ),
    ];
    for (id, columns) in expected {
        let k = nutsh_catalog::kind(id).unwrap_or_else(|| panic!("no kind {id}"));
        assert!(k.curated, "{id} has no curated.toml entry");
        // Header and path together: a header assertion alone passes when the path beside it is
        // a typo, and a typo renders as a column of `-` that no other test can see.
        let got: Vec<(&str, &str)> = k.columns.iter().map(|c| (c.header, c.path)).collect();
        assert_eq!(&got, columns, "{id}");
    }
    // The row tint follows the first `Status` column, so SEVERITY has to be both first and
    // `Status`: the header assertion alone would let a later `Status` column above it take
    // the colour. `status` sits second and stays a `Status` column deliberately.
    let alert = nutsh_catalog::kind("monitoring.serviceability.Alert").unwrap();
    assert_eq!(alert.columns[0].header, "SEVERITY");
    assert_eq!(alert.columns[0].kind, nutsh_catalog::ColumnKind::Status);
    // The standalone NIC child kind moves to the non-deprecated spellings with the VM's.
    let nic = nutsh_catalog::kind("vmm.ahv.config.Nic").unwrap();
    let paths: Vec<&str> = nic.columns.iter().map(|c| c.path).collect();
    assert_eq!(
        paths,
        [
            "nicNetworkInfo.nicType",
            "nicBackingInfo.macAddress",
            "nicNetworkInfo.ipv4Config.ipAddress.value",
            "nicNetworkInfo.subnet.extId",
        ]
    );
}

#[test]
fn the_curated_action_set_is_bound_and_scoped() {
    let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").unwrap();
    let by_key = |k: &str| {
        vm.actions
            .iter()
            .find(|a| a.key == k)
            .unwrap_or_else(|| panic!("no VM action bound to {k:?}"))
    };
    assert_eq!(by_key("p").name, "power-on");
    assert_eq!(by_key("P").name, "power-off");
    assert_eq!(by_key("r").name, "reboot");
    assert_eq!(by_key("m").name, "migrate-to-host");
    assert_eq!(by_key("C").name, "clone");
    assert_eq!(by_key("ctrl-d").name, "delete");
    let snapshot = by_key("s");
    assert_eq!(snapshot.name, "snapshot");
    assert!(
        snapshot.path.starts_with("/dataprotection/"),
        "snapshotting is a POST to recovery points naming the VM, not a VM action"
    );
    assert_eq!(snapshot.form.len(), 3);
    assert_eq!(snapshot.form[1].value, Some("$ext_id"));
    assert_eq!(snapshot.form[2].value, Some("$now+30d"));
    assert_eq!(
        vm.action("delete").unwrap().confirm,
        nutsh_catalog::ConfirmKind::TypeName
    );
    assert_eq!(
        vm.action("power-off").unwrap().confirm,
        nutsh_catalog::ConfirmKind::Yes
    );

    let alert = nutsh_catalog::kind("monitoring.serviceability.Alert").unwrap();
    assert!(alert.action("manage-alert").unwrap().hidden);
    let ack = alert.action("acknowledge").unwrap();
    assert_eq!(ack.key, "A");
    assert_eq!(ack.body, Some(r#"{"actionType":"ACKNOWLEDGE"}"#));
    assert_eq!(ack.path, alert.action("manage-alert").unwrap().path);
    assert_eq!(alert.action("resolve").unwrap().key, "R");

    // §5.4: the spec declares no If-Match for either, and the overlay says so.
    let task = nutsh_catalog::kind("prism.config.Task").unwrap();
    assert_eq!(task.action("cancel").unwrap().key, "c");
    assert!(!task.action("cancel").unwrap().needs_etag);
    let hosts = nutsh_catalog::kind("clustermgmt.config.Host~hosts").unwrap();
    assert!(!hosts.action("enter-host-maintenance").unwrap().needs_etag);
    assert_eq!(hosts.action("enter-host-maintenance").unwrap().key, "M");

    // The flat Host borrows them and says where the cluster id comes from.
    let host = nutsh_catalog::kind("clustermgmt.config.Host").unwrap();
    assert_eq!(host.action_kind, Some("clustermgmt.config.Host~hosts"));
    assert_eq!(host.action_parents, ["cluster.uuid"]);
    assert_eq!(host.action_target().id, "clustermgmt.config.Host~hosts");

    // Storage containers carry no `extId` at all.
    let sc = nutsh_catalog::kind("clustermgmt.config.StorageContainer").unwrap();
    assert_eq!(sc.ext_id_key, "containerExtId");
}
