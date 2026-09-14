use std::collections::{HashMap, HashSet};

use nutsh_catalog::{
    ACTION_LABEL, DEFAULT_MAX_ROWS, DETAIL_LABEL, KINDS, NAMESPACES, kind, lookup,
    placeholder_count,
};

#[test]
fn catalog_is_populated() {
    assert!(NAMESPACES.len() >= 19, "{} namespaces", NAMESPACES.len());
    assert!(KINDS.len() >= 200, "{} kinds", KINDS.len());
}

#[test]
fn ids_are_unique() {
    let mut seen = HashSet::new();
    for k in KINDS {
        assert!(seen.insert(k.id), "duplicate id {}", k.id);
    }
}

#[test]
fn paths_and_parents_are_consistent() {
    for k in KINDS {
        assert!(k.list_path.starts_with('/'), "{}", k.id);
        assert!(
            k.list_path
                .starts_with(&format!("/{}/{}/", k.namespace, k.version)),
            "{} path {}",
            k.id,
            k.list_path
        );
        assert!(
            !k.fallback_columns.is_empty(),
            "{} has no fallback columns",
            k.id
        );
        if let Some(p) = k.parent {
            assert!(kind(p).is_some(), "{} has unknown parent {p}", k.id);
        }
        if let Some(g) = k.get_path {
            assert!(g.ends_with('}'), "{} get path {g}", k.id);
        }
        for a in k.actions {
            // An action sits on the kind's own list path, or the second pass claimed it: a
            // `$actions` POST whose prefix ends in the same two segments as the get path,
            // placeholder names normalised (`/operations/clusters/{clusterExtId}/hosts/{extId}`
            // for a `/config/clusters/{clusterExtId}/hosts/{extId}` get path). An
            // `extra_actions` claim outside that rule must extend this test.
            let claimed = k.get_path.is_some_and(|g| {
                a.path
                    .split_once("/$actions/")
                    .is_some_and(|(prefix, _)| trailing_two(prefix) == trailing_two(g))
            });
            // Or a `from` copy across kinds - the VM's `snapshot` is RecoveryPoint's `create`
            // - which sits on the source kind's list path, where the source still owns it.
            // `Action` records no provenance, so this matches by path and method rather than
            // by the `from` that made the copy: any action sitting on another kind's list path
            // that the other kind declares with the same path and method passes. Tighten it to
            // compare against a source field if one is ever added to `Action`.
            let copied = KINDS.iter().any(|source| {
                source.id != k.id
                    && a.path.starts_with(source.list_path)
                    && source
                        .actions
                        .iter()
                        .any(|s| s.path == a.path && s.method == a.method)
            });
            assert!(
                a.path.starts_with(k.list_path) || claimed || copied,
                "{} action {} path {}",
                k.id,
                a.name,
                a.path
            );
        }
    }
}

/// The last segment of a dotted column path: `diskAdvanceConfig.isOnline` → `isOnline`. The
/// property a column actually reads, which is what the name rules below ask about.
fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// The last two segments of a path with every `{name}` read as `{}`.
fn trailing_two(path: &str) -> Vec<&str> {
    path.rsplit('/')
        .take(2)
        .map(|s| if s.starts_with('{') { "{}" } else { s })
        .collect()
}

#[test]
fn every_namespace_has_kinds() {
    for n in NAMESPACES {
        assert!(
            KINDS.iter().any(|k| k.namespace == n.name),
            "namespace {} has no kinds",
            n.name
        );
    }
}

#[test]
fn curated_aliases_resolve() {
    assert_eq!(lookup("vm")[0].id, "vmm.ahv.config.Vm");
    assert_eq!(lookup("cluster")[0].id, "clustermgmt.config.Cluster");
    assert_eq!(lookup("host")[0].id, "clustermgmt.config.Host");
    assert_eq!(lookup("task")[0].id, "prism.config.Task");
    assert_eq!(lookup("alert")[0].id, "monitoring.serviceability.Alert");
    assert_eq!(lookup("subnet")[0].id, "networking.config.Subnet");
    // A curated kind outranks a derived alias that happens to match first.
    assert_eq!(lookup("vms")[0].id, "vmm.ahv.config.Vm");
    assert_eq!(lookup("hosts")[0].id, "clustermgmt.config.Host");
    assert!(lookup("zzz-no-such-kind").is_empty());
}

#[test]
fn curated_flag_matches_overlay() {
    let curated: Vec<&str> = KINDS.iter().filter(|k| k.curated).map(|k| k.id).collect();
    // Twelve arrived with the hand-picked columns of the Compute & Storage and Hardware sidebar
    // groups, whose thirteenth kind, the ESXi VM, was already curated for its display name;
    // twenty-four more with those of Network & Security, Data Protection and Policies, whose
    // other two, the VPC and the virtual switch, are the pair curated for
    // `warm` alone; sixteen more with those of Monitoring and Admin Center. That last group
    // also gave columns to the five kinds that had been curated for a flag alone - users and
    // the domain manager for `warm`, audits, events and LCM histories for a row budget or an
    // order - so the only kinds curated without columns are now the four `VmReference` display
    // names, which are the same nested schema under four paths and open no table of their own.
    // One more for the bucket.
    assert_eq!(curated.len(), 79, "{curated:?}");
    let flagged: Vec<&str> = curated
        .iter()
        .copied()
        .filter(|id| kind(id).unwrap().columns.is_empty())
        .collect();
    assert_eq!(
        flagged,
        [
            "vmm.ahv.policies.VmReference",
            "vmm.ahv.policies.VmReference~dependee-vms",
            "vmm.ahv.policies.VmReference~dependent-vms",
            "vmm.ahv.policies.VmReference~dependent-vms~2",
        ],
        "curated for something other than columns"
    );
    assert!(kind("vmm.ahv.config.Vm").unwrap().curated);
    // Curated for columns alone, with no display name or aliases of their own.
    assert_eq!(kind("vmm.ahv.config.Disk").unwrap().columns.len(), 4);
    assert_eq!(kind("vmm.ahv.config.Nic").unwrap().columns.len(), 4);
}

#[test]
fn vm_actions_follow_the_etag_rule() {
    let vm = kind("vmm.ahv.config.Vm").unwrap();
    for name in ["power-on", "power-off", "reboot", "delete", "update"] {
        let a = vm
            .action(name)
            .unwrap_or_else(|| panic!("no action {name}"));
        assert!(a.needs_etag, "{name} must need an ETag");
    }
    assert!(!vm.action("create").unwrap().needs_etag);
    let power_on = vm.action("power-on").unwrap();
    assert!(!power_on.takes_body && !power_on.needs_body);
    // `clone` declares its body without `required: true`, so it is taken but not needed;
    // `guest-shutdown` declares one that is required.
    let clone = vm.action("clone").unwrap();
    assert!(clone.takes_body && !clone.needs_body);
    let shutdown = vm.action("guest-shutdown").unwrap();
    assert!(shutdown.takes_body && shutdown.needs_body);
    assert!(
        vm.action("power-on")
            .unwrap()
            .roles
            .contains(&"Administrator")
    );
}

#[test]
fn actions_attach_across_placeholder_renames() {
    // Real specs write `/clusters/{clusterExtId}/$actions/...` under a `/clusters/{extId}` get path.
    assert!(
        kind("clustermgmt.config.Cluster")
            .unwrap()
            .action("expand-cluster")
            .is_some()
    );
    assert!(
        kind("prism.config.Task")
            .unwrap()
            .action("cancel")
            .is_some()
    );
    assert!(
        kind("networking.config.Subnet")
            .unwrap()
            .action("share")
            .is_some()
    );
}

/// `parents_needed` over the shipped catalog rather than a hand-built `Action`: a `create`
/// POSTs to a list path and names no entity, so a *nested* create still needs its parent id,
/// while an entity action needs every placeholder but the last.
#[test]
fn parents_needed_counts_everything_but_the_entity() {
    let vm = kind("vmm.ahv.config.Vm").unwrap();
    let power_off = vm.action("power-off").unwrap();
    assert!(power_off.names_entity());
    assert_eq!(power_off.parents_needed(), 0);
    let create = vm.action("create").unwrap();
    assert!(!create.names_entity(), "a create posts to a list path");
    assert_eq!(create.parents_needed(), 0);
    let nic = kind("vmm.ahv.config.Nic").unwrap();
    let nic_create = nic.action("create").unwrap();
    assert!(!nic_create.names_entity());
    assert_eq!(
        nic_create.parents_needed(),
        1,
        "a nested create still needs its parent"
    );
    let nic_delete = nic.action("delete").unwrap();
    assert!(nic_delete.names_entity());
    assert_eq!(nic_delete.parents_needed(), 1);
}

/// `action_target` falls back to the kind itself when `action_kind` names nothing, so a typo
/// in `curated.toml` would route a kind's actions silently. Vacuous until a kind is curated
/// to borrow; it fails the moment a curated value is misspelled.
#[test]
fn action_kind_names_a_known_kind() {
    for k in KINDS {
        if let Some(id) = k.action_kind {
            assert!(
                kind(id).is_some(),
                "{} borrows actions from unknown {id}",
                k.id
            );
            assert_eq!(k.action_target().id, id, "{} does not reach {id}", k.id);
        }
    }
}

/// What `parents_needed`, `fill_placeholders` and the mock handler rely on: `scope` names
/// every placeholder of the path, and a body is only ever required where one is taken.
#[test]
fn action_scope_and_body_flags_are_consistent() {
    for k in KINDS {
        for a in k.actions {
            assert_eq!(
                a.scope.len(),
                placeholder_count(a.path),
                "{} {}: scope {:?} against path {}",
                k.id,
                a.name,
                a.scope,
                a.path
            );
            assert!(
                !a.needs_body || a.takes_body,
                "{} {} needs a body it does not take",
                k.id,
                a.name
            );
        }
    }
}

/// Every kind is bounded, and the ones that are not bounded at the default are named here
/// with their number, beside the order that decides which rows the budget keeps.
///
/// Both halves are deliberate. The first is what the budget is for:
/// `monitoring.serviceability.Audit` holds 181 825 rows, 1819 pages at the measured 0.85 s a
/// page, and unbudgeted the walk runs to the scheduler's blanket 200-page cap - 170 s of every
/// 30 s cycle, and an empty table throughout. The second is a list a future edit has to come
/// and change on purpose rather than by accident.
#[test]
fn the_unbounded_kinds_carry_a_row_budget_and_an_order() {
    assert_eq!(DEFAULT_MAX_ROWS, 1000, "ten pages of 100");

    // Nothing reaches the catalog unbudgeted: the generator writes `DEFAULT_MAX_ROWS` into
    // every kind `curated.toml` does not give a number of its own.
    for k in KINDS {
        assert!(k.max_rows.is_some(), "{}: no row budget", k.id);
        assert!(k.max_rows != Some(0), "{}: a budget of no rows", k.id);
        // An order the endpoint takes no `$orderby` for would be dropped by the client.
        assert!(
            k.orderby.is_none() || k.list_params.orderby,
            "{}: orderby with no $orderby parameter",
            k.id
        );
        // A budget on a kind that does not page is inert rather than wrong - the walk ends
        // after one page either way - so 43 singleton-ish kinds carry one and never use it.
    }

    // The budgets `curated.toml` sets by hand, and their numbers. A Prism Central never trims
    // these collections: the recording holds 6109 tasks, 1559 alerts and 181 825 audits, so a table
    // over one is capped at the newest 500 rather than walking the whole of it every cycle.
    let curated: Vec<(&str, u32)> = KINDS
        .iter()
        .filter(|k| k.max_rows != Some(DEFAULT_MAX_ROWS))
        .map(|k| (k.id, k.max_rows.expect("every kind is budgeted")))
        .collect();
    assert_eq!(
        curated,
        [
            ("monitoring.serviceability.Alert", 500),
            ("monitoring.serviceability.Audit", 500),
            ("prism.config.Task", 500),
        ]
    );

    // The curated orders, and every field in them is in its endpoint's `x-odata-fields`
    // allowlist in the spec this catalog pins; `curated.toml` cites the line for each.
    let ordered: Vec<(&str, &str)> = KINDS
        .iter()
        .filter_map(|k| k.orderby.map(|o| (k.id, o)))
        .collect();
    assert_eq!(
        ordered,
        [
            ("lifecycle.resources.LcmHistory", "startTime desc"),
            ("monitoring.serviceability.Alert", "creationTime desc"),
            ("monitoring.serviceability.Audit", "creationTime desc"),
            ("monitoring.serviceability.Event", "creationTime desc"),
            ("prism.config.Task", "createdTime desc"),
        ]
    );

    // A budget below the default keeps a tenth of what the others keep, so it may not keep an
    // arbitrary tenth: every hand-set budget names the order that decides which rows survive.
    // The rest stay reachable - the header prints `500/181825`, a pane may name its own
    // order, and the filter grammar will narrow by time when it lands.
    for (id, _) in &curated {
        assert!(
            kind(id).unwrap().orderby.is_some(),
            "{id} caps its rows without saying which rows to keep"
        );
    }
}

/// `probe_by` is data in `generated.rs`: inspectable, diffable, refutable by one run against a Prism Central. The
/// proven set is two kinds, both with `lastUpdatedTime`, the field `fixtures-lab` shows on
/// every row of both. Everything else is `None` until someone has evidence.
#[test]
fn only_the_two_proven_kinds_carry_a_probe_field() {
    let armed: Vec<(&str, &str)> = KINDS
        .iter()
        .filter_map(|k| Some((k.id, k.probe_by?)))
        .collect();
    assert_eq!(
        armed,
        [
            ("monitoring.serviceability.Alert", "lastUpdatedTime"),
            ("prism.config.Task", "lastUpdatedTime"),
        ]
    );
    // And neither kind's curated order moved: the probe buys its monotone order without
    // spending the table's.
    let task = kind("prism.config.Task").expect("Tasks");
    assert_eq!(task.orderby, Some("createdTime desc"));
    let alert = kind("monitoring.serviceability.Alert").expect("Alerts");
    assert_eq!(alert.orderby, Some("creationTime desc"));
    // A probe needs both parameters, whatever the overlay says.
    for k in KINDS.iter().filter(|k| k.probe_by.is_some()) {
        assert!(k.list_params.orderby && k.list_params.limit, "{}", k.id);
    }
}

/// The warm set is the ten kinds the spec derived plus the gateway, and every one of them can
/// be listed and has a name to cache. The count is asserted because the flag is cheap to add
/// and each one costs a request at every connect.
///
/// The gateway is the eleventh on the spec's own evidence rule (§4.3 counts `Reference` paths
/// inside the default six): curation put `localGatewayReference` and `remoteGatewayReference`
/// in the first six columns of both `networking.config.VpnConnection` and
/// `networking.config.BgpSession`, which is four - the count that warms `vpc.extId` and
/// `host.extId` - where the spec counted the pre-curation `localGateway.extId` as a singleton.
#[test]
fn the_warm_set_is_eleven_listable_named_kinds() {
    let warm: Vec<&str> = KINDS.iter().filter(|k| k.warm).map(|k| k.id).collect();
    assert_eq!(
        warm,
        vec![
            "clustermgmt.config.Cluster",
            "clustermgmt.config.Host",
            "clustermgmt.config.StorageContainer",
            "iam.authn.User",
            "networking.config.Gateway",
            "networking.config.Subnet",
            "networking.config.VirtualSwitch",
            "networking.config.Vpc",
            "prism.config.Category",
            "prism.config.DomainManager",
            "vmm.content.Image",
        ]
    );
    for k in KINDS.iter().filter(|k| k.warm) {
        assert!(
            matches!(nutsh_catalog::reach(k), nutsh_catalog::Reach::Direct),
            "{} is warm but not directly listable",
            k.id
        );
    }
}

/// Every kind whose name is not at the top level of its JSON, and where it is instead. The list
/// is asserted whole so a fifth is a deliberate decision rather than a copy-paste.
#[test]
fn the_kinds_that_name_themselves_off_the_top_level() {
    let mut nested: Vec<(&str, &str)> = KINDS
        .iter()
        .filter(|k| !k.name_path.is_empty())
        .map(|k| (k.id, k.name_path))
        .collect();
    nested.sort();
    assert_eq!(
        nested,
        vec![
            // A PCIe device has no name; `description` is its model string.
            ("clustermgmt.ahv.config.PcieDevice", "description"),
            // Both GPU profiles keep their identity under `configuration`.
            (
                "clustermgmt.ahv.config.PhysicalGpuProfile",
                "configuration.name"
            ),
            (
                "clustermgmt.ahv.config.VirtualGpuProfile",
                "configuration.name"
            ),
            // A disk carries `hostName`, which `NAME_KEYS` would take for its own.
            ("clustermgmt.config.Disk", "serialNumber"),
            // The sync policy's schema has no `name`: `entityName` is the identity.
            ("datapolicies.config.EntitySyncPolicy", "entityName"),
            // `entityModel` is the thing; `entityType` is a class of thing.
            ("lifecycle.resources.Entity", "entityModel"),
            // The Prism Central's own row: the name is at `config.name`.
            ("prism.config.DomainManager", "config.name"),
        ]
    );
}

/// The label → kind items of the named `nav.toml` groups whose kind has no curated columns.
/// Exit criterion 2 is that this comes back empty for all nine curated groups; Tasks 6, 7 and 8
/// each add their own groups to the list below as they land.
///
/// Every requested name must match a group. Without that a misspelled name - `"Data
/// protection"` for `"Data Protection"` - would contribute no items and the caller's
/// `assert!(missing.is_empty())` would pass while checking nothing at all.
fn uncurated_items_of(groups: &[&str]) -> Vec<String> {
    let mut missing = Vec::new();
    let mut seen = Vec::new();
    for g in &nutsh_catalog::NAV[..nutsh_catalog::CURATED_GROUPS] {
        if !groups.contains(&g.name) {
            continue;
        }
        seen.push(g.name);
        for item in g.items {
            let nutsh_catalog::NavTarget::Kind(id) = item.target else {
                continue;
            };
            let k = kind(id).unwrap_or_else(|| panic!("{id} is not in the catalog"));
            if k.columns.is_empty() {
                missing.push(format!("{} ({id})", item.label));
            }
        }
    }
    for g in groups {
        assert!(
            seen.contains(g),
            "no curated nav group is named {g:?}; the names are {:?}",
            nutsh_catalog::NAV[..nutsh_catalog::CURATED_GROUPS]
                .iter()
                .map(|g| g.name)
                .collect::<Vec<_>>()
        );
    }
    missing
}

/// Exit criterion 2: every item of the curated sidebar groups opens a table whose columns were
/// chosen by hand, not by a schema heuristic.
#[test]
fn every_item_of_the_curated_groups_has_curated_columns() {
    let missing = uncurated_items_of(&[
        "Compute & Storage",
        "Hardware",
        "Network & Security",
        "Data Protection",
        "Policies",
        "Monitoring",
        "Admin Center",
    ]);
    assert!(missing.is_empty(), "not curated: {missing:?}");
}

/// No curated column shows a raw identifier as text. A `Reference` resolves to a name and
/// stubs to eight dim characters when it cannot; a `Text` column over an extId is 36 characters
/// of nothing, on every row, for ever.
#[test]
fn no_curated_column_shows_an_identifier_as_text() {
    for k in KINDS {
        for c in k.columns {
            let last = last_segment(c.path);
            let identifier = last == "extId"
                || last == "uuid"
                || last.ends_with("ExtId")
                || last.ends_with("Uuid")
                || last.ends_with("Reference");
            assert!(
                !identifier
                    || matches!(
                        c.kind,
                        nutsh_catalog::ColumnKind::Reference | nutsh_catalog::ColumnKind::Count
                    ),
                "{}: column {:?} over {} is {:?}",
                k.id,
                c.header,
                c.path,
                c.kind
            );
        }
    }
}

/// The generator's own checks run at generation time; these are the same facts asserted against
/// the committed catalog, so a hand-edit of `generated.rs` is caught by `cargo test` as well as
/// by the drift check.
#[test]
fn no_curated_status_is_past_the_sixth_column() {
    for k in KINDS {
        for (i, c) in k.columns.iter().enumerate() {
            assert!(
                c.kind != nutsh_catalog::ColumnKind::Status || i < nutsh_catalog::DEFAULT_COLUMNS,
                "{}: Status column {:?} at index {i}",
                k.id,
                c.header
            );
        }
    }
}

/// The row's colour is the *first* `Status` column of the shown set: `table::status_index`
/// picks it, and `ui::render_rows` paints that one cell with `role_fg` and every other cell of
/// the row with `row_fg` of the same word. So the leading `Status` column is the one a row's
/// colour means, and a kind that leads with a field under an optional object tints its rows
/// off an absence. `multidomain.config.RegisteredDomain` is the one that did: `platformData`
/// is optional and `connectivityStatus` an unconstrained string, so a Prism Central that
/// answers without it draws `-`, which normalises to `_`, and a `REGISTRATION_ERROR` row came
/// out Muted grey. `registrationState` leads instead - an enum every word of which the kind's
/// `[kinds.*.status]` block maps - and the Disaster Recovery page's pane, which the generator
/// holds to a subsequence of these columns, therefore leads with it too.
#[test]
fn a_registered_prism_central_takes_its_colour_from_its_registration_state() {
    let k = kind("multidomain.config.RegisteredDomain").expect("the catalog has registered PCs");
    let first = k
        .columns
        .iter()
        .find(|c| c.kind == nutsh_catalog::ColumnKind::Status)
        .expect("three of them");
    assert_eq!(first.header, "STATE");
    assert_eq!(first.path, "registrationState");
}

/// `projectExtId` can never resolve - there is no `Project` kind, because `listProjects` exists
/// only in the beta `specs/multidomain/v4.4.b1.yaml` - and it was empty on every recorded row
/// inspected.
#[test]
fn no_column_anywhere_shows_a_project_ext_id() {
    for k in KINDS {
        for c in k.columns.iter().chain(k.fallback_columns) {
            assert_ne!(c.path, "projectExtId", "{}", k.id);
        }
    }
}

/// No column carries a credential. The generator drops them; this says so about the shipped
/// file, curated columns included.
///
/// `Bool` and `Count` are the two kinds allowed to keep a secret-sounding name, because
/// neither renders the value: a flag *about* a credential
/// (`isForceResetPasswordEnabled`, `hasPrivateKey`, `shouldValidateAdCredential`) and the
/// length of a list of them (`bucketsAccessKeys`). That is the same line `is_secret_property`
/// draws when it asks the name rule of `string` properties only.
#[test]
fn no_column_anywhere_is_a_secret() {
    use nutsh_catalog::ColumnKind;
    for k in KINDS {
        for c in k.columns.iter().chain(k.fallback_columns) {
            let last = last_segment(c.path);
            assert!(
                !nutsh_catalog::secret::is_secret_name(last)
                    || matches!(c.kind, ColumnKind::Bool | ColumnKind::Count),
                "{}: {} is a {:?} column and looks like a secret",
                k.id,
                c.path,
                c.kind
            );
        }
    }
}

/// Every curated detail label fits the pane's label column, every section has a field, and no
/// kind carries two sections with the same title. The generator refuses all three, so this is
/// the assertion that the generator ran - and the one that fails if anyone hand-edits
/// `generated.rs`.
#[test]
fn curated_details_fit_the_pane() {
    for k in KINDS {
        assert!(
            k.detail.len() <= 12,
            "{}: {} sections",
            k.id,
            k.detail.len()
        );
        let mut titles = HashSet::new();
        for s in k.detail {
            assert!(
                titles.insert(s.title),
                "{}: two sections titled {:?}",
                k.id,
                s.title
            );
            assert!(
                !s.fields.is_empty(),
                "{}: section {:?} has no fields",
                k.id,
                s.title
            );
            assert!(
                s.fields.len() <= 24,
                "{}: section {:?} has {} fields",
                k.id,
                s.title,
                s.fields.len()
            );
            for f in s.fields {
                assert!(
                    f.label.chars().count() <= DETAIL_LABEL,
                    "{}: label {:?} is {} characters",
                    k.id,
                    f.label,
                    f.label.chars().count()
                );
            }
        }
    }
    // Pinned to the VM, not to a whole-catalog total: a total goes on rising as more kinds are
    // curated and would stop saying anything about the kind it was written for.
    let vm = kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
    let titles: Vec<&str> = vm.detail.iter().map(|s| s.title).collect();
    assert_eq!(
        titles,
        [
            "Identity",
            "State",
            "Compute",
            "UEFI boot",
            "Legacy boot",
            "Storage",
            "Network",
            "Protection",
            "Placement",
            "Guest",
            "Lifecycle",
        ],
        "the VM's eleven sections, in curated order"
    );
}

/// The eleven kinds the readable-views spec curates. A kind that loses its `detail` block - to a
/// bad rebase of `curated.toml`, which is the way it would happen - fails here rather than
/// showing a fallback nobody notices is a fallback.
#[test]
fn the_eleven_curated_kinds_have_a_detail() {
    for id in [
        "vmm.ahv.config.Vm",
        "clustermgmt.config.Host",
        "clustermgmt.config.Cluster",
        "clustermgmt.config.StorageContainer",
        "networking.config.Subnet",
        "monitoring.serviceability.Alert",
        "prism.config.Task",
        "dataprotection.config.RecoveryPoint",
        "datapolicies.config.ProtectionPolicy",
        "volumes.config.VolumeGroup",
        "vmm.content.Image",
    ] {
        let k = kind(id).unwrap_or_else(|| panic!("no kind {id}"));
        assert!(!k.detail.is_empty(), "{id} has no curated detail");
    }
}

/// Every action label the menu draws fits the narrowest box it is drawn in, and no kind binds
/// one key to two actions.
///
/// The generator refuses both; this is the shipped constant agreeing with it. A rule checked
/// only where the TOML is read says nothing about the catalog the program links, and the two
/// can drift.
///
/// `title()`, not `label`: an uncurated action draws its catalog name, and
/// `resume-synchronous-replication` is thirty cells of it.
#[test]
fn every_action_label_fits_its_box_and_no_kind_binds_a_key_twice() {
    for k in KINDS {
        let mut keys: HashMap<&str, &str> = HashMap::new();
        for a in k.actions {
            let width = a.title().chars().count();
            assert!(
                width <= ACTION_LABEL,
                "{}: action label {:?} is {width} characters, at most {ACTION_LABEL}",
                k.id,
                a.title()
            );
            if a.key.is_empty() {
                continue;
            }
            if let Some(other) = keys.insert(a.key, a.name) {
                panic!("{}: key {:?} runs both {other} and {}", k.id, a.key, a.name);
            }
        }
    }
}

/// A user has to be able to tell, from the menu alone, which of the eight power verbs cuts
/// power and which asks the guest to stop. Getting that wrong on a database is not a mistake
/// the second attempt fixes, and `Reset` beside `Reboot` beside `Power cycle` says nothing.
///
/// The wording is checked here rather than left to review because the catalog is where it is
/// written and the spec is where the meaning comes from: `power-off` is "Force power off",
/// `reset` is "without waiting for the guest", `shutdown` is "through the ACPI support" and
/// `guest-shutdown` is "requesting Nutanix Guest Tools".
#[test]
fn every_vm_power_verb_states_its_consequence() {
    let vm = kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
    for (name, expect) in [
        ("power-off", "cuts power"),
        ("power-cycle", "cuts power"),
        ("reset", "cuts power"),
        ("shutdown", "ACPI"),
        ("reboot", "ACPI"),
        ("guest-shutdown", "guest tools"),
        ("guest-reboot", "guest tools"),
    ] {
        let action = vm.action(name).unwrap_or_else(|| panic!("VMs have {name}"));
        assert!(
            action.title().contains(expect),
            "{name} is labelled {:?}, which does not say {expect:?}",
            action.title()
        );
    }
    assert_eq!(
        vm.action("power-on").expect("VMs power on").title(),
        "Power on",
        "the one verb with no consequence to state"
    );
}

/// The graceful pair had no key at all, so the only way to reach them was the menu. They take
/// the shifted half of the keys their hypervisor-level siblings hold: `d`/`D` shut down,
/// `r`/`R` reboot, lower case through ACPI and upper case through the guest tools.
#[test]
fn the_guest_verbs_have_keys_paired_with_their_acpi_siblings() {
    let vm = kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
    for (name, key) in [
        ("shutdown", "d"),
        ("guest-shutdown", "D"),
        ("reboot", "r"),
        ("guest-reboot", "R"),
    ] {
        let action = vm.action(name).unwrap_or_else(|| panic!("VMs have {name}"));
        assert_eq!(action.key, key, "{name}");
    }
}

/// The head of a dotted path, which is what `$select` names: `nics[].networkInfo` is `nics`.
fn select_root(path: &str) -> &str {
    path.split(['.', '[']).next().unwrap_or(path).trim()
}

/// A kind's `$select` roots, or `None` where it is not narrowed.
fn select_roots(k: &'static nutsh_catalog::Kind) -> Option<HashSet<&'static str>> {
    k.select.map(|s| s.split(',').map(str::trim).collect())
}

/// Everything a **table row** is read for reaches the wire.
///
/// `$select` is what makes a hundred virtual machines a hundred kilobytes instead of five
/// hundred, and it is also the one change that can blank a cell without anybody noticing: the
/// property simply is not there, and every renderer treats an absent value as an absent value.
/// So this enumerates the readers, and it is the list the generator builds the union from.
/// Adding a new read off a row means adding it in both places.
#[test]
fn select_names_every_property_a_row_is_read_for() {
    for k in KINDS {
        let Some(roots) = select_roots(k) else {
            continue;
        };
        let mut want: Vec<(&str, String)> = vec![("the identifier", k.ext_id_key.to_string())];
        if !k.name_path.is_empty() {
            want.push((
                "the curated name path",
                select_root(k.name_path).to_string(),
            ));
        }
        // `tui::table::columns`: curated columns replace the derived ones rather than
        // extending them, and `w` widens within whichever set is drawn.
        let shown = if k.columns.is_empty() {
            k.fallback_columns
        } else {
            k.columns
        };
        want.extend(
            shown
                .iter()
                .map(|c| ("a column", select_root(c.path).to_string())),
        );
        // `core::search::addresses`: an IP is matched wherever the kind keeps one, including a
        // detail field, because a Host's IPMI address is a detail field and nothing else.
        want.extend(
            k.columns
                .iter()
                .chain(k.fallback_columns)
                .filter(|c| c.kind == nutsh_catalog::ColumnKind::Ip)
                .map(|c| ("an address column", select_root(c.path).to_string())),
        );
        want.extend(
            k.detail
                .iter()
                .flat_map(|s| s.fields)
                .filter(|f| f.kind == nutsh_catalog::ColumnKind::Ip)
                .map(|f| ("an address detail field", select_root(f.path).to_string())),
        );
        // `scheduler::list` reads it off every row of the walk to calibrate the change probe.
        if let Some(field) = k.probe_by {
            want.push(("probe_by", select_root(field).to_string()));
        }
        // `core::actions::plan` fills an action's leading placeholders from the row.
        want.extend(
            k.action_parents
                .iter()
                .map(|p| ("an action parent", select_root(p).to_string())),
        );
        // `core::actions::cancelable`.
        if k.action("cancel").is_some() {
            want.push(("isCancelable", "isCancelable".to_string()));
        }
        for (why, root) in want {
            assert!(
                roots.contains(root.as_str()),
                "{}: {why} {root:?} is outside $select {:?}",
                k.id,
                k.select
            );
        }
    }
}

/// A page pane may draw a column the kind's own table does not, over the same narrowed list.
#[test]
fn select_names_every_column_a_page_pane_draws() {
    for page in nutsh_catalog::PAGES {
        for pane in page.panes {
            let k = kind(pane.kind).unwrap_or_else(|| panic!("{}: no kind {}", page.id, pane.kind));
            let Some(roots) = select_roots(k) else {
                continue;
            };
            for c in pane.columns {
                assert!(
                    roots.contains(select_root(c.path)),
                    "{}: {}'s column {:?} is outside {}'s $select {:?}",
                    page.id,
                    pane.title,
                    c.path,
                    k.id,
                    k.select
                );
            }
        }
    }
}

/// A kind with no single-entity GET is never narrowed.
///
/// The detail is composed from the document the store holds, and the only thing that can hand
/// the store a whole one is a `get_in`. Narrow a kind that has none and everything the columns
/// do not name is gone for the session, with nothing able to fetch it back.
#[test]
fn a_kind_with_no_get_path_is_never_narrowed() {
    let unfetchable: Vec<&str> = KINDS
        .iter()
        .filter(|k| k.get_path.is_none() && k.select.is_some())
        .map(|k| k.id)
        .collect();
    assert!(unfetchable.is_empty(), "{unfetchable:?}");
}

/// Every root asked for is a property the schema declares. A `$select` naming one that is not
/// is a 400 on the whole list, and a table lost is worse than a payload unnarrowed.
#[test]
fn select_never_names_a_property_the_endpoint_does_not_declare() {
    for k in KINDS {
        let Some(roots) = select_roots(k) else {
            continue;
        };
        assert!(
            k.list_params.select,
            "{}: narrowed, but the endpoint declares no $select",
            k.id
        );
        assert!(!roots.is_empty(), "{}: an empty $select", k.id);
        assert!(
            roots.iter().all(|r| !r.is_empty() && !r.contains(' ')),
            "{}: {:?}",
            k.id,
            k.select
        );
    }
}

/// The narrowing is worth having: the kinds a table spends its session on are among them.
#[test]
fn the_kinds_a_session_spends_its_time_on_are_narrowed() {
    for id in [
        "vmm.ahv.config.Vm",
        "prism.config.Task",
        "monitoring.serviceability.Alert",
        "monitoring.serviceability.Audit",
        "monitoring.serviceability.Event",
        "networking.config.Subnet",
    ] {
        let k = kind(id).unwrap_or_else(|| panic!("no kind {id}"));
        assert!(k.select.is_some(), "{id} is not narrowed");
    }
}
