//! Composition without a terminal: `core::detail` returns plain data, so every rule the pane
//! draws is asserted here rather than through a frame.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nutsh_catalog::{ColumnKind, DetailField, DetailSection, kind};
use nutsh_core::cell::Names;
use nutsh_core::detail::{self, Section};
use nutsh_prism::Entity;
use serde_json::{Value, json};

/// 2026-09-05T10:00:00Z, the instant the snapshot tests freeze.
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_788_602_400)
}

fn vm(raw: Value) -> Entity {
    Entity::new(
        kind("vmm.ahv.config.Vm").expect("the catalog has VMs"),
        raw,
        None,
    )
}

fn titles(sections: &[Section]) -> Vec<&str> {
    sections.iter().map(|s| s.title.as_str()).collect()
}

/// A section's fields as `(label, text)`: what every assertion below reads.
fn pairs(s: &Section) -> Vec<(&str, &str)> {
    s.fields
        .iter()
        .map(|(l, r)| (l.as_str(), r.text.as_str()))
        .collect()
}

/// A section's labels, for the two fallback assertions that care about the list and not the
/// values in it.
fn labels(s: &Section) -> Vec<&str> {
    s.fields.iter().map(|(l, _)| l.as_str()).collect()
}

const ALL_MISSING: &[DetailField] = &[
    DetailField {
        label: "A",
        path: "nope",
        kind: ColumnKind::Text,
        when: None,
    },
    DetailField {
        label: "B",
        path: "also.nope",
        kind: ColumnKind::Reference,
        when: None,
    },
];
const ALL_FALSE: &[DetailField] = &[
    DetailField {
        label: "A",
        path: "a",
        kind: ColumnKind::Bool,
        when: None,
    },
    DetailField {
        label: "B",
        path: "b",
        kind: ColumnKind::Bool,
        when: None,
    },
];
const NAME: &[DetailField] = &[DetailField {
    label: "Name",
    path: "name",
    kind: ColumnKind::Text,
    when: None,
}];

/// The rule that makes it safe to curate generously: a section is kept iff at least one of its
/// fields rendered to something other than the dim `-`. `✗` and `0` are values, so a section of
/// all-false flags is drawn.
#[test]
fn a_section_of_dashes_disappears_and_a_section_of_crosses_stays() {
    let e = vm(json!({"a": false, "b": false}));
    let sections = detail::curated(
        &[
            DetailSection {
                title: "Gone",
                fields: ALL_MISSING,
                when: None,
            },
            DetailSection {
                title: "Flags",
                fields: ALL_FALSE,
                when: None,
            },
        ],
        &e,
        &Names::default(),
        now(),
    );
    assert_eq!(titles(&sections), ["Flags"]);
    assert_eq!(pairs(&sections[0]), [("A", "✗"), ("B", "✗")]);
}

/// The three `when` forms, each dropping a section whose fields do have values - which is what
/// tells a failing condition apart from an empty section.
#[test]
fn a_when_that_does_not_hold_drops_a_section_whose_fields_have_values() {
    let e = vm(json!({"name": "web-01"}));
    let with = |when: &'static str| {
        detail::curated(
            &[DetailSection {
                title: "Guest",
                fields: NAME,
                when: Some(when),
            }],
            &e,
            &Names::default(),
            now(),
        )
    };
    assert_eq!(titles(&with("name")), ["Guest"]);
    assert!(with("guestTools").is_empty(), "absent, so the section goes");
    assert_eq!(titles(&with("!guestTools")), ["Guest"]);
    assert!(with("!name").is_empty());
    assert_eq!(
        titles(&with("name = WEB-01")),
        ["Guest"],
        "compared case-insensitively, so an enum may be written the way a person writes it"
    );
    assert!(with("name = db-01").is_empty());
    // A field's own `when` uses the same evaluator: the field goes, and with it the section.
    const CONDITIONAL: &[DetailField] = &[DetailField {
        label: "Name",
        path: "name",
        kind: ColumnKind::Text,
        when: Some("guestTools"),
    }];
    assert!(
        detail::curated(
            &[DetailSection {
                title: "Guest",
                fields: CONDITIONAL,
                when: None
            }],
            &e,
            &Names::default(),
            now(),
        )
        .is_empty()
    );
}

/// The flagship variant sections, over the real catalog: the tag is compared on its short name,
/// so a build that spells it `vmm.v4.r0.b1.ahv.config.UefiBoot` and one that spells it
/// `vmm.v4.ahv.config.UefiBoot` both find the same section.
#[test]
fn the_two_boot_sections_are_mutually_exclusive_on_one_entity() {
    let vms = kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
    let names = Names::default();
    let legacy = detail::compose(
        vms,
        &vm(json!({
            "extId": "3d0c4a2e-1b8f-4c1a-9e2f-000000000001",
            "name": "web-01",
            "powerState": "ON",
            "bootConfig": {
                "$objectType": "vmm.v4.ahv.config.LegacyBoot",
                "bootOrder": ["CDROM", "DISK"]
            }
        })),
        &names,
        now(),
    );
    assert!(
        titles(&legacy).contains(&"Legacy boot"),
        "{:?}",
        titles(&legacy)
    );
    assert!(!titles(&legacy).contains(&"UEFI boot"));

    let uefi = detail::compose(
        vms,
        &vm(json!({
            "extId": "3d0c4a2e-1b8f-4c1a-9e2f-000000000002",
            "name": "web-02",
            "powerState": "ON",
            "bootConfig": {
                "$objectType": "vmm.v4.r0.b1.ahv.config.UefiBoot",
                "isSecureBootEnabled": true
            }
        })),
        &names,
        now(),
    );
    assert!(titles(&uefi).contains(&"UEFI boot"), "{:?}", titles(&uefi));
    assert!(!titles(&uefi).contains(&"Legacy boot"));

    // No `bootConfig` at all: both sections go, and the ones that do not depend on it stay, in
    // curated order.
    let bare = detail::compose(
        vms,
        &vm(json!({"extId": "x", "name": "web-03", "powerState": "ON"})),
        &names,
        now(),
    );
    assert!(
        !titles(&bare).iter().any(|t| t.ends_with("boot")),
        "{:?}",
        titles(&bare)
    );
    assert_eq!(&titles(&bare)[..2], &["Identity", "State"]);
}

/// A composed field is a table cell: the fan-out rule, the duplicate collapse and the `+n` come
/// from `render_value` and are not reimplemented here.
#[test]
fn a_composed_fan_out_collapses_duplicates_and_counts_the_rest() {
    const IPS: &[DetailField] = &[DetailField {
        label: "IPv4",
        path: "nics[].nicNetworkInfo.ipv4Config.ipAddress.value",
        kind: ColumnKind::Ip,
        when: None,
    }];
    let e = vm(json!({"nics": [
        {"nicNetworkInfo": {"ipv4Config": {"ipAddress": {"value": "192.0.2.135"}}}},
        {"nicNetworkInfo": {"ipv4Config": {"ipAddress": {"value": "192.0.2.135"}}}},
        {"nicNetworkInfo": {"ipv4Config": {"ipAddress": {"value": "192.0.2.102"}}}}
    ]}));
    let sections = detail::curated(
        &[DetailSection {
            title: "Network",
            fields: IPS,
            when: None,
        }],
        &e,
        &Names::default(),
        now(),
    );
    assert_eq!(pairs(&sections[0]), [("IPv4", "192.0.2.135 +1")]);
}

/// A kind nobody curated a *detail* for gets its table row stood up, with the identifier the
/// table never shows put in front of it - from its curated columns where it has them, and from
/// the columns the generator derived where it has none.
#[test]
fn an_uncurated_kind_gets_its_table_row_stood_up() {
    let audits = kind("monitoring.serviceability.Audit").expect("the catalog has Audits");
    let e = Entity::new(
        audits,
        json!({
            "extId": "0f6a4b58-9f9b-4b58-9f9b-000000000020",
            "serviceName": "aplos",
            "operationType": "CREATE",
            "status": "SUCCESS"
        }),
        None,
    );
    let sections = detail::compose(audits, &e, &Names::default(), now());
    assert_eq!(sections[0].title, "Summary");
    assert_eq!(labels(&sections[0])[0], "UUID");
    assert_eq!(
        sections[0].fields[0].1.text, "0f6a4b58-9f9b-4b58-9f9b-000000000020",
        "whole, not an eight-character stub: this is the identifier, not a reference to one"
    );
    // Audits carry curated columns and no curated detail, so the Summary is that column list,
    // header by header: `OPERATION` → `Operation`, `STATUS` → `Status`.
    let audit = pairs(&sections[0]);
    assert!(audit.contains(&("Operation", "Create")), "{audit:?}");
    assert!(audit.contains(&("Status", "SUCCESS")), "{audit:?}");

    // A kind with neither: the derived list, whole rather than the six a table draws, since a
    // detail has no `w` and the seventh column has nowhere else to be read.
    let pools = kind("clustermgmt.config.IpPool").expect("the catalog has IP pools");
    let e = Entity::new(
        pools,
        json!({
            "extId": "0f6a4b58-9f9b-4b58-9f9b-000000000021",
            "name": "pool-a",
            "type": "PE_IP_POOL",
            "isInUse": false,
            "ranges": [{"startAddress": {"value": "192.0.2.10"}}]
        }),
        None,
    );
    let sections = detail::compose(pools, &e, &Names::default(), now());
    assert_eq!(
        pairs(&sections[0]),
        [
            ("UUID", "0f6a4b58-9f9b-4b58-9f9b-000000000021"),
            ("Name", "pool-a"),
            ("Type", "PE IP pool"),
            ("Is in use", "✗"),
            ("Ranges", "1"),
        ]
    );
}

/// One synthetic entity carrying one of everything §4.5 has a rule for.
fn everything() -> Value {
    json!({
        "extId": "0006158a-2f0d-4d5a-8e2d-000000000010",
        "$objectType": "mini.v4.config.Thing",
        "links": [{"href": "/api/things/1"}],
        "tenantId": "0006158a-2f0d-4d5a-8e2d-0000000000ff",
        "name": "thing-01",
        "powerState": "POWERED_OFF",
        "createTime": "2026-09-05T09:00:00Z",
        "memorySizeBytes": 8589934592u64,
        "capabilities": ["VSS_SNAPSHOT", "SELF_SERVICE_RESTORE"],
        "cluster": {
            "$objectType": "mini.v4.config.ClusterReference",
            "extId": "0006237e-c983-e712-0000-00000001c1d0"
        },
        // An Alert's, an Event's and an Audit's own shape: one object holding one array of
        // scalars, which the fan-out reads through `impactedEntities.clusters[]`.
        "impactedEntities": {
            "$objectType": "mini.v4.common.ImpactedEntities",
            "clusters": ["0006237e-c983-e712-0000-00000001c1d0"]
        },
        "guestTools": {
            "isInstalled": true,
            "version": "4.5",
            "guestInfo": {"guestOsFullName": "Ubuntu 24.04", "dnsName": {"value": "thing-01.lab"}}
        },
        "disks": [
            {
                "$objectType": "mini.v4.config.VmDisk",
                "extId": "d1",
                "diskAddress": {"busType": "SCSI", "index": 0},
                "backingInfo": {"diskSizeBytes": 88046829568u64, "serialId": "s-1", "isMigrationInProgress": false}
            },
            {"extId": "d2", "diskAddress": {"busType": "SCSI", "index": 1}},
            {"extId": "d3", "diskAddress": {"busType": "IDE", "index": 0}},
            {"extId": "d4", "diskAddress": {"busType": "IDE", "index": 1}}
        ]
    })
}

/// The four rules of §4.5, over one entity, with a kind whose curation cannot get in the way:
/// `monitoring.serviceability.Audit` curates no columns, so its Summary is derived and its
/// top-level keys are all left to rules 2 to 4.
#[test]
fn the_fallback_groups_the_document_and_drops_the_noise() {
    let audits = kind("monitoring.serviceability.Audit").expect("the catalog has Audits");
    let e = Entity::new(audits, everything(), None);
    let mut names = Names::default();
    names.insert(
        "0006237e-c983-e712-0000-00000001c1d0".into(),
        "lab-cluster".into(),
    );
    let sections = detail::fallback(audits, &e, &names, now());
    let names_of = titles(&sections);

    // Rule 1, then rule 2, then one section per object and per array of objects, in wire order.
    assert_eq!(names_of[0], "Summary", "{names_of:?}");
    assert_eq!(names_of[1], "Details", "{names_of:?}");
    assert!(names_of.contains(&"Guest tools"), "{names_of:?}");
    assert!(names_of.contains(&"Disks (4)"), "{names_of:?}");

    // The four noise keys appear nowhere, as a label or as a value.
    for section in &sections {
        for (label, value) in &section.fields {
            for noise in ["$objectType", "$reserved", "links", "tenantId"] {
                assert!(!label.contains(noise), "{label:?} in {}", section.title);
                assert!(!value.text.contains(noise), "{:?}", value.text);
            }
        }
        assert!(!section.fields.is_empty(), "{} is empty", section.title);
    }

    // Rule 2: the top-level scalars, the array of scalars through the fan-out, and the one-leaf
    // object inlined as a dotted field - with the reference resolved.
    let details = sections
        .iter()
        .find(|s| s.title == "Details")
        .expect("Details");
    let got = pairs(details);
    assert!(got.contains(&("Name", "thing-01")), "{got:?}");
    assert!(
        got.contains(&("Power state", "Powered off")),
        "a SCREAMING_SNAKE string is inferred as an `Enum`, which is sentence case: {got:?}"
    );
    assert!(
        got.contains(&("Memory size bytes", "8 GiB")),
        "inferred Bytes: {got:?}"
    );
    assert!(
        got.contains(&("Create time", "1h")),
        "inferred Timestamp: {got:?}"
    );
    assert!(
        got.contains(&("Capabilities", "Vss snapshot +1")),
        "an array of scalars through the fan-out: {got:?}"
    );
    assert!(
        got.contains(&("Cluster · extId", "lab-cluster")),
        "a one-leaf object is inlined, and its UUID-shaped value inferred as a Reference: {got:?}"
    );
    assert!(
        got.contains(&("Impacted entities · clusters", "lab-cluster")),
        "an inlined fan-out leaf drops the `[]`: a label is a name, not a path expression: {got:?}"
    );
    assert!(
        !got.iter().any(|(l, _)| *l == "Guest tools"),
        "four leaves: its own section"
    );

    // Rule 3: leaf-segment labels, flattened however deep the leaves were.
    let guest = sections
        .iter()
        .find(|s| s.title == "Guest tools")
        .expect("Guest tools");
    assert_eq!(
        pairs(guest),
        [
            ("Is installed", "✓"),
            ("Version", "4.5"),
            ("Guest os full name", "Ubuntu 24.04"),
            ("Value", "thing-01.lab"),
        ]
    );

    // Rule 4: the first three elements, one line each, tagged on the first and reaching depth 2
    // for the scalars a depth-1 rule would have missed.
    let disks = sections
        .iter()
        .find(|s| s.title == "Disks (4)")
        .expect("Disks (4)");
    let lines = pairs(disks);
    assert_eq!(lines.len(), 3, "the first three of four: {lines:?}");
    assert_eq!(
        lines[0].0, "VM disk",
        "the variant tag labels the line it is on"
    );
    assert_eq!(
        lines[1].0, "",
        "this PC emits the tag on the first element only, and a repeat is dropped anyway"
    );
    assert!(
        lines[0].1.starts_with("d1  SCSI  0  "),
        "depth 2, name then extId then wire order: {lines:?}"
    );
    assert!(
        lines[0].1.ends_with("… and 2 more"),
        "six leaves, four shown: {lines:?}"
    );
    // `Ide`, not `IDE`: an inferred `Enum` is spelled by `words::sentence_case`, whose acronym
    // table holds `Scsi` and not `Ide`. The casing of a wire word belongs to that table, which
    // every view shares, and not to this walk.
    assert_eq!(lines[2].1, "d3  Ide  0");
}

/// The label rule, on the two shapes that break a leaf-segment-only one. Deviation 3.
#[test]
fn colliding_leaf_labels_grow_left_until_they_differ() {
    let audits = kind("monitoring.serviceability.Audit").expect("the catalog has Audits");
    let e = Entity::new(
        audits,
        json!({"controllerVm": {
            "externalAddress": {"ipv4": {"value": "192.0.2.31", "prefixLength": 24}},
            "backplaneAddress": {"ipv4": {"value": "192.168.5.2", "prefixLength": 24}},
            "isInMaintenanceMode": false
        }}),
        None,
    );
    let sections = detail::fallback(audits, &e, &Names::default(), now());
    let section = sections
        .iter()
        .find(|s| s.title == "Controller VM")
        .unwrap_or_else(|| panic!("{:?}", titles(&sections)));
    assert_eq!(
        labels(section),
        [
            "External address · ipv4.value",
            "External address · ipv4.prefixLength",
            "Backplane address · ipv4.value",
            "Backplane address · ipv4.prefixLength",
            "Is in maintenance mode",
        ],
        "only the colliding labels grow, and only far enough"
    );
}

/// A curated column that reaches into an object takes that one leaf and no more. The shape is a
/// lab audit's: `sourceEntity` holds three leaves and the SOURCE column names one of them. Taking
/// the whole key would cost exactly the kinds someone bothered to curate their richest sections -
/// a lab cluster's `config` is 25 leaves behind one dotted column - and §4.5 rule 3 asks for a
/// section per top-level object, not a section per uncurated one.
#[test]
fn a_curated_column_takes_its_own_leaf_and_leaves_the_siblings() {
    let audits = kind("monitoring.serviceability.Audit").expect("the catalog has Audits");
    let e = Entity::new(
        audits,
        json!({
            "extId": "0f6a4b58-9f9b-4b58-9f9b-000000000023",
            "sourceEntity": {
                "$objectType": "monitoring.v4.serviceability.AuditEntityReference",
                "type": "Token",
                "name": "source-01",
                "extId": "0f6a4b58-9f9b-4b58-9f9b-000000000024"
            }
        }),
        None,
    );
    let sections = detail::fallback(audits, &e, &Names::default(), now());
    let source = sections
        .iter()
        .find(|s| s.title == "Source entity")
        .unwrap_or_else(|| panic!("{:?}", titles(&sections)));
    assert_eq!(
        labels(source),
        ["Type", "Ext ID"],
        "the Summary shows `sourceEntity.name`, so the section shows its siblings and not it"
    );
}

/// The rule-4 drop, on the shape the recording actually holds: an authorization policy's `entities[]`
/// is one `entityFilter` whose only scalar is four levels down, so every element line renders the
/// dim `-` and the section goes rather than being drawn as a title over blank lines. The filter
/// map is what the raw view (`Y`) is for.
#[test]
fn an_array_whose_elements_say_nothing_at_depth_two_drops_its_section() {
    let policies = kind("iam.authz.AuthorizationPolicy").expect("the catalog has policies");
    let e = Entity::new(
        policies,
        json!({
            "extId": "0f6a4b58-9f9b-4b58-9f9b-000000000022",
            "displayName": "predefined-read-only",
            "entities": [{"entityFilter": {"*": {"*": {"eq": "*"}}}}]
        }),
        None,
    );
    let sections = detail::fallback(policies, &e, &Names::default(), now());
    assert!(
        !titles(&sections).contains(&"Entities (1)"),
        "{:?}",
        titles(&sections)
    );
}

/// Every `.json` under the three fixture trees - the curated one and the two recorded Prism
/// Centrals - with the kind its path names.
fn fixture_entities() -> Vec<(&'static nutsh_catalog::Kind, Vec<Value>)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "json") {
                out.push(path);
            }
        }
    }
    /// `…/vmm/v4.3/ahv/config/vms/<uuid>/disks.json` → `/vmm/v4.3/ahv/config/vms/{}/disks`,
    /// which is `Kind::list_path` with its placeholder names normalised.
    fn shape(rest: &str) -> String {
        rest.trim_end_matches(".json")
            .split('/')
            .map(|s| {
                if s.contains('-') && s.len() == 36 {
                    "{}"
                } else {
                    s
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }
    fn normalise(list_path: &str) -> String {
        list_path
            .split('/')
            .map(|s| if s.starts_with('{') { "{}" } else { s })
            .collect::<Vec<_>>()
            .join("/")
    }
    // The one fixture that answers an endpoint no kind lists is still a recorded document, and
    // the walk has to hold over it too, so it runs under the dataprotection kind the catalog
    // *does* name: a kind decides rule 1 and nothing else, and rules 2 to 4 over the document
    // are what this file asserts.
    let orphan = kind("dataprotection.config.RecoveryPoint").expect("the catalog has those");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../mockpc");
    let mut out = Vec::new();
    let mut kindless = Vec::new();
    for tree in ["fixtures", "fixtures-lab", "fixtures-lab-sierra"] {
        let dir = root.join(tree);
        let mut files = Vec::new();
        walk(&dir, &mut files);
        // Per tree, not over the total: a tree that vanished - renamed, gitignored, or a
        // `read_dir` that failed and was swallowed by `walk` - would otherwise leave a corpus
        // four fifths smaller and every assertion below still passing.
        assert!(!files.is_empty(), "{tree} contributed no fixtures");
        for file in files {
            let rest = format!(
                "/{}",
                file.strip_prefix(&dir).expect("under the tree").display()
            );
            let want = shape(&rest);
            let listed = nutsh_catalog::KINDS
                .iter()
                .find(|k| normalise(k.list_path) == want);
            if listed.is_none() {
                kindless.push(rest);
            }
            let text = std::fs::read_to_string(&file).expect("readable fixture");
            let entities: Vec<Value> = serde_json::from_str(&text).expect("a JSON array");
            out.push((listed.unwrap_or(orphan), entities));
        }
    }
    // One fixture answers an endpoint no kind lists: `protected-resources` is a `{extId}` GET
    // and nothing else (`core::sampler`'s own first paragraph says so), so the Disaster Recovery
    // page samples it by identifier. A second entry here means a fixture was added under a path
    // no `Kind::list_path` matches, which is a bug in the task that added it.
    kindless.sort();
    assert_eq!(
        kindless,
        ["/dataprotection/v4.4/config/protected-resources.json"],
        "every other fixture is some kind's listing"
    );
    out
}

/// The two properties that actually guard §4.5, over every recorded entity there is:
/// `fallback` never panics, never emits a noise key and never emits an empty section; and no two
/// labels within one section are equal, which is the assertion that fails if anyone
/// reintroduces dotted labels or a runtime truncation to `DETAIL_LABEL`.
#[test]
fn the_fallback_holds_over_every_recorded_fixture() {
    let names = Names::default();
    for (kind, entities) in fixture_entities() {
        for raw in entities {
            let e = Entity::new(kind, raw, None);
            for section in detail::fallback(kind, &e, &names, now()) {
                // Empty in both senses: no fields, and no field that says anything. The second
                // is what `push_if_any` maintains, so it is asserted the way that rule states
                // it - not one field is the dim `-` every absence renders as. A rule-4 section
                // whose elements hold nothing at depth two is the case (the recording's
                // `iam.authz.AuthorizationPolicy` has one), and a title over blank lines is
                // exactly the noise this view exists to replace.
                assert!(
                    section
                        .fields
                        .iter()
                        .any(|(_, r)| !(r.dim && r.text == "-")),
                    "{}: {} says nothing",
                    kind.id,
                    section.title
                );
                let mut seen = std::collections::HashSet::new();
                for (label, value) in &section.fields {
                    for noise in ["$objectType", "$reserved", "links", "tenantId"] {
                        assert!(!label.contains(noise), "{}: {label:?}", kind.id);
                    }
                    // An element line of a rule-4 section has no label of its own past the
                    // first, so the blank one is not a collision.
                    assert!(
                        label.is_empty() || seen.insert(label.clone()),
                        "{}: two fields labelled {label:?} in {} ({})",
                        kind.id,
                        section.title,
                        value.text
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `$select`: what a projection costs the composed detail.
//
// These two settle, with the composer rather than by argument, whether a `$select` derived
// from the curated *columns* is safe. The detail renders out of the store row (`App::
// detail_entity` reads `store.table(key).rows[ext_id]`), and a list cycle replaces that map
// wholesale (`Store::apply`, `Update::Complete`: `table.rows = index(s.entities)`), so the
// row the detail composes from is a list row for most of every poll interval.
// ---------------------------------------------------------------------------

/// The first segment of a dotted or indexed path - what an OData `$select` projects.
fn root_of(path: &str) -> &str {
    let end = path.find(['.', '[']).unwrap_or(path.len());
    &path[..end]
}

/// `raw` with only the named top-level properties kept: what a Prism Central returns for a
/// `$select` naming exactly those.
fn project(raw: &Value, keep: &[&str]) -> Value {
    let map = raw.as_object().expect("an object");
    Value::Object(
        map.iter()
            .filter(|(k, _)| keep.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

/// One whole Tasks row, shaped like `crates/mockpc/fixtures/prism/v4.4/config/tasks.json`.
fn task_row() -> Value {
    json!({
        "$objectType": "prism.v4.config.Task",
        "extId": "ZXJnb24=:11111111-1111-4111-8111-000000000001",
        "operation": "VmPowerOn",
        "operationDescription": "Power on VM",
        "status": "SUCCEEDED",
        "progressPercentage": 100,
        "createdTime": "2026-09-05T09:58:00Z",
        "startedTime": "2026-09-05T09:58:01Z",
        "completedTime": "2026-09-05T09:58:20Z",
        "lastUpdatedTime": "2026-09-05T09:58:20Z",
        "isCancelable": false,
        "numberOfSubtasks": 0,
        "numberOfEntitiesAffected": 1,
        "ownedBy": { "extId": "u1", "name": "admin" },
        "parentTask": { "extId": "ZXJnb24=:00000000-0000-4000-8000-000000000009" },
        "entitiesAffected": [
            { "extId": "3d0c4a2e-1b8f-4c1a-9e2f-000000000001", "rel": "vmm:ahv:config:vm", "name": "web-01" }
        ],
        "completionDetails": [ { "name": "hostExtId", "value": "7b2f2f70" } ],
    })
}

/// The projection §10.2's recipe derives for Tasks: `ext_id_key`, the root of every curated
/// column, the `orderby` field and `probe_by`. Spelled here rather than read off `Kind::select`
/// so the assertion holds whatever the generator ends up emitting.
fn column_derived_select(k: &'static nutsh_catalog::Kind) -> Vec<&'static str> {
    let mut keep = vec![k.ext_id_key];
    keep.extend(k.columns.iter().map(|c| root_of(c.path)));
    keep.extend(
        [k.orderby, k.probe_by]
            .into_iter()
            .flatten()
            .map(|o| root_of(o.split(' ').next().unwrap_or(o))),
    );
    keep.sort_unstable();
    keep.dedup();
    keep
}

/// **The check the release plan asks for**: open `y` on a narrowed kind and assert a field the
/// columns do not show.
///
/// A `$select` derived from the columns alone keeps the Tasks table pixel-identical and takes
/// four of the detail's five sections down with it. `push_if_any` drops a section whose every
/// field rendered `-`, so this is not dimming: whole headings vanish.
#[test]
fn a_column_derived_select_costs_the_curated_detail_its_sections() {
    let tasks = kind("prism.config.Task").expect("the catalog has Tasks");
    let keep = column_derived_select(tasks);
    let whole = task_row();
    let thin = project(&whole, &keep);

    let full = detail::compose(
        tasks,
        &Entity::new(tasks, whole, None),
        &Names::default(),
        now(),
    );
    let narrowed = detail::compose(
        tasks,
        &Entity::new(tasks, thin, None),
        &Names::default(),
        now(),
    );

    // Every field the columns do show survives - that is why the *table* cannot catch this.
    assert!(titles(&full).contains(&"What"), "{:?}", titles(&full));
    // And the fields they do not show are gone. `operation` and `ownedBy.name` are in the
    // "What" section and in no column; `parentTask` is the whole of what is left of
    // "Hierarchy" once `numberOfSubtasks` goes too.
    let what = full.iter().find(|s| s.title == "What").expect("What");
    let operation = pairs(what)
        .into_iter()
        .find(|(l, _)| *l == "Operation")
        .expect("the whole row renders Operation");
    assert_ne!(operation.1, "-", "{operation:?}");
    let what = narrowed
        .iter()
        .find(|s| s.title == "What")
        .expect("What survives: Status and Progress are columns");
    // The section survives - Status and Progress *are* columns - and the two fields that are
    // not columns render the dim `-` that means "the Prism Central did not send this".
    for blanked in ["Operation", "Owner"] {
        let (_, text) = pairs(what)
            .into_iter()
            .find(|(l, _)| *l == blanked)
            .unwrap_or_else(|| panic!("{blanked} is curated"));
        assert_eq!(
            text,
            "-",
            "a column-derived $select was expected to blank {blanked}: {:?}",
            pairs(what)
        );
    }

    assert!(
        titles(&narrowed).len() < titles(&full).len(),
        "sections lost: {:?} -> {:?}",
        titles(&full),
        titles(&narrowed)
    );
    for gone in ["Hierarchy", "Result"] {
        assert!(titles(&full).contains(&gone), "{gone} is curated");
        assert!(
            !titles(&narrowed).contains(&gone),
            "{gone} survives a column-derived $select, so this test proves nothing: {:?}",
            titles(&narrowed)
        );
    }
}

/// The same measurement for VMs, where the saving is worth having and the loss is worst: ten
/// column roots against the thirty-four the curated detail, its `when` predicates and
/// `action_parents` read between them.
#[test]
fn the_same_holds_for_the_kind_the_projection_is_written_for() {
    let vms = kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
    let keep = column_derived_select(vms);
    let detail_roots: Vec<&str> = vms
        .detail
        .iter()
        .flat_map(|s| s.fields.iter())
        .map(|f| root_of(f.path))
        .collect();
    let missing: Vec<&str> = detail_roots
        .iter()
        .copied()
        .filter(|r| !keep.contains(r))
        .collect();
    assert!(
        missing.len() > 15,
        "the columns were expected to cover far less of the detail than this: {missing:?}"
    );
}

/// Volume groups and images have no fixture in either tree, so their curation is asserted over a
/// hand-built entity whose keys are the schema's own top-level property names: every curated path
/// resolves to the value put there, and no section is silently empty. Stated as a limitation
/// rather than papered over - a recording should replace this.
#[test]
fn the_two_unrecorded_kinds_render_every_curated_path() {
    let cases: [(&str, Value); 2] = [
        (
            "volumes.config.VolumeGroup",
            json!({
                "extId": "9f9b0f6a-4b58-4b58-9f9b-000000000031",
                "name": "vg-lab-02",
                "description": "iSCSI target",
                "createdBy": "operator",
                "projectExtId": "0006158a-2f0d-4d5a-8e2d-000000000010",
                "sharingStatus": "SHARED",
                "hydrationStatus": "COMPLETE",
                "usageType": "USER",
                "attachmentType": "DIRECT",
                "protocol": "ISCSI",
                "isHidden": false,
                "shouldLoadBalanceVmAttachments": true,
                "targetName": "vg-lab-02-tgt",
                "clusterReference": "0006158a-2f0d-4d5a-8e2d-000000000010",
                "attachments": [{}, {}]
            }),
        ),
        (
            "vmm.content.Image",
            json!({
                "extId": "7521370a-d3d9-426a-9bdc-000000000041",
                "name": "ubuntu-24.04-cloud",
                "description": "cloud image",
                "type": "DISK_IMAGE",
                "ownerName": "operator",
                "projectExtId": "0006158a-2f0d-4d5a-8e2d-000000000010",
                "sizeBytes": 3221225472u64,
                "checksum": {"hexDigest": "0f6a4b589f9b"},
                "contentRepositoryName": "default",
                "clusterLocationExtIds": ["0006158a-2f0d-4d5a-8e2d-000000000010"],
                "placementPolicyStatus": [{"complianceStatus": "COMPLIANT"}],
                "categoryExtIds": ["0006158a-2f0d-4d5a-8e2d-000000000012"],
                "isSharedWithAllProjects": true,
                "createTime": "2026-09-05T09:00:00Z",
                "lastUpdateTime": "2026-09-05T09:30:00Z"
            }),
        ),
    ];
    for (id, raw) in cases {
        let k = kind(id).unwrap_or_else(|| panic!("no kind {id}"));
        let e = Entity::new(k, raw, None);
        let sections = detail::compose(k, &e, &Names::default(), now());
        assert_eq!(
            sections.len(),
            k.detail.len(),
            "{id}: no curated section is silently empty: {:?}",
            titles(&sections)
        );
        for section in &sections {
            for (label, value) in pairs(section) {
                assert_ne!(
                    value, "-",
                    "{id}: {} · {label} resolves to nothing",
                    section.title
                );
            }
        }
    }
}
