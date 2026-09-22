//! The demo estate holds together: what the library embeds is what the tree on disk says, every
//! id points at a row, the story's numbers are the numbers, every table the sidebar and the
//! pages open has rows, and the load-time rules (rebase, derived children) do what they claim.
//!
//! One `Site` row per embedded site. PR3 adds `demo-dr` here and nowhere else in this file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nutsh_catalog::{CURATED_GROUPS, Kind, NAV, NavTarget, PAGES};
use nutsh_mockpc::Store;
use nutsh_mockpc::demo::{DEMO, DEMO_DR, parse_rfc3339_z, store, t0};
use serde_json::Value;

/// `pc-ridge`'s domain manager, as the harbor site's `registered-domains` and every
/// `*domainManagerExtId` pointing at the ridge name it; `pc-ridge`'s own `domain-managers.json`
/// carries this id.
const RIDGE_DOMAIN_MANAGER: &str = "d0000000-0b02-4000-8000-000000000001";

/// `pc-harbor`'s domain manager, which the ridge site's `registered-domains` and every
/// `*domainManagerExtId` pointing at the harbor name.
const HARBOR_DOMAIN_MANAGER: &str = "d0000000-0a01-4000-8000-000000000001";

const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";

struct Site {
    name: &'static str,
    table: &'static [(&'static str, &'static str)],
    /// Namespaces this site deliberately does not serve; kinds under them are exempt from the
    /// coverage gate here and are what `src/demo.rs` marks unavailable at boot.
    absent: &'static [&'static str],
    /// Kinds the story leaves without a row on this site although the namespace is served:
    /// exempt from the coverage gate on this site only. Spec §4's file list gives them `0`.
    empty: &'static [&'static str],
    sibling_domain_manager: &'static str,
}

const SITES: &[Site] = &[
    Site {
        name: "demo",
        table: DEMO,
        absent: &["aiops", "storage", "tenancy", "objects"],
        empty: &[],
        sibling_domain_manager: RIDGE_DOMAIN_MANAGER,
    },
    Site {
        name: "demo-dr",
        table: DEMO_DR,
        absent: &[
            "aiops", "storage", "tenancy", "files", "opsmgmt", "security",
        ],
        // `licensing` is served and answers 403, so its three kinds have no rows; the rest
        // are the `0` cells of spec §4's demo-dr column.
        empty: &[
            "licensing.config.License",
            "licensing.config.Entitlement",
            "licensing.config.Compliance",
            "networking.config.NicProfile",
            "clustermgmt.ahv.config.PhysicalGpuProfile",
            "clustermgmt.ahv.config.VirtualGpuProfile",
            "vmm.content.Ova",
            "vmm.esxi.config.Vm",
            "iam.authn.SamlIdentityProvider",
        ],
        sibling_domain_manager: HARBOR_DOMAIN_MANAGER,
    },
];

fn tree(site: &Site) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures-demo")
        .join(site.name)
}

fn json_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            json_files(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(
                path.strip_prefix(root)
                    .expect("under root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

/// A table's store at `T0`: delta zero, so the rows are exactly the files plus the derived
/// lists.
fn at_t0(site: &Site) -> Store {
    store(site.table, t0()).unwrap_or_else(|e| panic!("{}: {e}", site.name))
}

/// `Store` has no key iterator on purpose; the table gives every file's key.
fn key_of(path: &str) -> String {
    format!("/{}", path.strip_suffix(".json").unwrap_or(path))
}

/// `(key, value, path-for-the-message)` for every scalar in a store, derived lists included.
fn walk(store: &Store) -> Vec<(String, Value, String)> {
    fn visit(v: &Value, key: &str, at: &str, out: &mut Vec<(String, Value, String)>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    visit(child, k, &format!("{at}.{k}"), out);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    visit(child, key, &format!("{at}[{i}]"), out);
                }
            }
            scalar => out.push((key.to_string(), scalar.clone(), at.to_string())),
        }
    }
    let mut out = Vec::new();
    for path in store.paths() {
        for (i, row) in store.list(path).unwrap_or_default().iter().enumerate() {
            visit(row, "", &format!("{path}[{i}]"), &mut out);
        }
    }
    out
}

#[test]
fn embedded_tables_match_the_trees_on_disk() {
    for site in SITES {
        let root = tree(site);
        let mut on_disk = Vec::new();
        json_files(&root, &root, &mut on_disk);
        on_disk.sort();
        let mut in_table: Vec<String> = site.table.iter().map(|(p, _)| (*p).to_string()).collect();
        in_table.sort();
        assert_eq!(
            on_disk,
            in_table,
            "{}: the files under {} and the entries of the table are one list",
            site.name,
            root.display()
        );
        let mut deduped = in_table.clone();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            site.table.len(),
            "{}: a path is listed twice in the table",
            site.name
        );
        let loaded = Store::load(&root).unwrap_or_else(|e| panic!("{}: {e}", root.display()));
        let embedded =
            Store::from_files(site.table).unwrap_or_else(|e| panic!("{}: {e}", site.name));
        for path in &in_table {
            let key = key_of(path);
            assert_eq!(
                loaded.list(&key),
                embedded.list(&key),
                "{}: {path} differs between disk and the table",
                site.name
            );
        }
        assert!(
            loaded == embedded,
            "{}: disk and table build different stores",
            site.name
        );
        // The derived lists are not files: a child list on disk would be a second copy of the
        // parent's array, free to drift from it.
        for path in &in_table {
            let derived = (path.contains("/vms/")
                && [
                    "/disks.json",
                    "/nics.json",
                    "/cd-roms.json",
                    "/gpus.json",
                    "/serial-ports.json",
                ]
                .iter()
                .any(|s| path.ends_with(s)))
                || (path.contains("/clusters/") && path.ends_with("/hosts.json"))
                || (path.contains("/hosts/") && path.ends_with("/host-nics.json"))
                || path.ends_with("/affected-entities.json");
            assert!(
                !derived,
                "{}: {path} is derived at load and must not be a file",
                site.name
            );
        }
    }
}

#[test]
fn no_password_key_and_under_512k() {
    for site in SITES {
        let bytes: usize = site.table.iter().map(|(_, text)| text.len()).sum();
        assert!(
            bytes < 512 * 1024,
            "{}: {bytes} bytes embedded; the hard cap is 512 KiB",
            site.name
        );
        let s = at_t0(site);
        for (key, _, at) in walk(&s) {
            assert_ne!(
                key, "password",
                "{}: a key named password at {at}; nothing in the demo is a credential",
                site.name
            );
        }
    }
}

// ── cross-reference rule 1: the generic walk ──────────────────────────────────────────────

/// The mock's own path match (`handler.rs`, `same_shape`): a `{placeholder}` segment matches
/// any one segment, so a derived or hand-written child list is keyed the way the mock keys it.
fn same_shape(catalog_path: &str, api_path: &str) -> bool {
    let catalog: Vec<&str> = catalog_path.split('/').collect();
    let api: Vec<&str> = api_path.split('/').collect();
    catalog.len() == api.len()
        && catalog
            .iter()
            .zip(&api)
            .all(|(c, a)| c.starts_with('{') || c == a)
}

/// The key a row of `path` is identified by: the owning kind's `ext_id_key` (`containerExtId`
/// on storage containers, `name` on buckets), `extId` on a path no kind owns
/// (`protected-resources`).
fn id_key_of(path: &str) -> &'static str {
    let version = nutsh_catalog::version_in(path).unwrap_or("");
    nutsh_catalog::KINDS
        .iter()
        .find(|k| same_shape(&nutsh_catalog::with_version(k.list_path, version), path))
        .map_or("extId", |k| k.ext_id_key)
}

/// Every row's own id across every list, derived lists included.
fn ids_of(store: &Store) -> HashSet<String> {
    let mut ids = HashSet::new();
    for path in store.paths() {
        let key = id_key_of(path);
        for row in store.list(path).unwrap_or_default() {
            if let Some(id) = row[key].as_str() {
                ids.insert(id.to_string());
            }
        }
    }
    ids
}

fn is_id_key(k: &str) -> bool {
    k == "extId"
        || k == "uuid"
        || k.ends_with("ExtId")
        || k.ends_with("Uuid")
        || k.ends_with("UUID")
}

fn is_id_list_key(k: &str) -> bool {
    k.ends_with("ExtIds") || k.ends_with("Uuids")
}

fn names_a_domain_manager(k: &str) -> bool {
    k.to_ascii_lowercase().ends_with("domainmanagerextid")
}

/// Every string under an id-shaped key (array items inherit their array's key) is the row's
/// own id, the nil UUID, a row of this site, or, under a `*domainManagerExtId`, the sibling.
fn check_refs(
    v: &Value,
    key: &str,
    at: &str,
    own: &str,
    ids: &HashSet<String>,
    sibling: &str,
    bad: &mut Vec<String>,
) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                check_refs(child, k, &format!("{at}.{k}"), own, ids, sibling, bad);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                check_refs(child, key, &format!("{at}[{i}]"), own, ids, sibling, bad);
            }
        }
        Value::String(s) => {
            if !(is_id_key(key) || is_id_list_key(key)) {
                return;
            }
            if s == own || s == NIL_UUID || ids.contains(s.as_str()) {
                return;
            }
            if names_a_domain_manager(key) && s == sibling {
                return;
            }
            bad.push(format!("{at} = {s}"));
        }
        _ => {}
    }
}

#[test]
fn every_reference_resolves() {
    for site in SITES {
        let s = at_t0(site);
        let ids = ids_of(&s);
        let mut bad = Vec::new();
        for path in s.paths() {
            let key = id_key_of(path);
            for (i, row) in s.list(path).unwrap_or_default().iter().enumerate() {
                let own = row[key]
                    .as_str()
                    .unwrap_or_else(|| panic!("{}: {path}[{i}] carries no {key}", site.name));
                check_refs(
                    row,
                    "",
                    &format!("{path}[{i}]"),
                    own,
                    &ids,
                    site.sibling_domain_manager,
                    &mut bad,
                );
            }
        }
        assert!(
            bad.is_empty(),
            "{}: {} reference(s) name no row of the site:\n{}",
            site.name,
            bad.len(),
            bad.join("\n")
        );
    }
}

// ── cross-reference rule 2: the story pins ────────────────────────────────────────────────

/// A kind's list at the version the site's files carry; empty when the namespace is absent.
fn rows_of<'a>(s: &'a Store, kind: &Kind) -> &'a [Value] {
    let Some(version) = s.version_of(kind.namespace) else {
        return &[];
    };
    s.list(&nutsh_catalog::with_version(kind.list_path, &version))
        .unwrap_or_default()
}

fn kind(id: &str) -> &'static Kind {
    nutsh_catalog::kind(id).unwrap_or_else(|| panic!("{id} is not a catalog kind"))
}

fn count(rows: &[Value], pred: impl Fn(&Value) -> bool) -> usize {
    rows.iter().filter(|r| pred(r)).count()
}

/// `evidence::WINDOW` (`crates/core/src/evidence.rs`): either side of the task's start.
const WINDOW: Duration = Duration::from_secs(10 * 60);
/// Every timestamp but `expirationTime` is before this, so nothing reads as the future on a
/// clock that runs a minute behind.
const CUTOFF: &str = "2026-09-05T09:59:00Z";

const FAILED_POWER_ON: &str = "ZXJnb24=:7a5c0000-0a01-4000-8000-000000000001";
const WARNING_IN_WINDOW: &str = "0b000000-0a01-4000-8000-000000000003";
const CRITICAL_OUTSIDE: &str = "0b000000-0a01-4000-8000-000000000002";

/// The header's counters and the story's rows on the harbor site. PR3 adds `ridge_pins`.
fn harbor_pins(s: &Store) {
    let vms = rows_of(s, kind("vmm.ahv.config.Vm"));
    let hosts = rows_of(s, kind("clustermgmt.config.Host"));
    let clusters = rows_of(s, kind("clustermgmt.config.Cluster"));
    let alerts = rows_of(s, kind("monitoring.serviceability.Alert"));
    let tasks = rows_of(s, kind("prism.config.Task"));
    let on = |r: &Value| r["powerState"] == "ON";
    let off = |r: &Value| r["powerState"] == "OFF";
    // `&'static str`, not `&str`: a closure cannot return a closure that borrows its own
    // argument for an elided lifetime (`lifetime may not live long enough`); every severity
    // here is a literal anyway.
    let open =
        |sev: &'static str| move |r: &Value| r["severity"] == sev && r["isResolved"] == false;

    // `clusters 2 hosts 4 · vms 12 on/off 10/2 · alerts ⚠ 3 ✖ 2 · tasks ▶ 2 running`
    assert_eq!(clusters.len(), 2, "clusters");
    assert_eq!(hosts.len(), 4, "hosts");
    assert_eq!(vms.len(), 12, "vms");
    assert_eq!(count(vms, on), 10, "vms on");
    assert_eq!(count(vms, off), 2, "vms off");
    assert_eq!(count(alerts, open("CRITICAL")), 2, "critical unresolved");
    assert_eq!(count(alerts, open("WARNING")), 3, "warning unresolved");
    assert_eq!(
        count(tasks, |r| r["status"] == "RUNNING"),
        2,
        "running tasks"
    );
    assert_eq!(count(tasks, |r| r["status"] == "FAILED"), 2, "failed tasks");

    // The failed power-on names dev-scratch-02, which is OFF; OFF VMs carry no host.
    let failed = tasks
        .iter()
        .find(|t| t["extId"] == FAILED_POWER_ON)
        .expect("the failed VmPowerOn task");
    assert_eq!(failed["status"], "FAILED");
    assert_eq!(failed["operation"], "VmPowerOn");
    assert_eq!(failed["entitiesAffected"][0]["name"], "dev-scratch-02");
    let scratch = vms
        .iter()
        .find(|v| v["name"] == "dev-scratch-02")
        .expect("dev-scratch-02");
    assert_eq!(scratch["powerState"], "OFF");
    for vm in vms.iter().filter(|v| off(v)) {
        assert!(
            vm.get("host").is_none(),
            "{}: an OFF VM carries no host",
            vm["name"]
        );
    }
    assert!(
        failed["errorMessages"]
            .as_array()
            .is_some_and(|e| e.len() == 2)
    );
    assert!(failed["subSteps"].as_array().is_some_and(|e| !e.is_empty()));

    // The evidence window: exactly one alert inside startedTime ± 10 min, and it is the WARNING.
    let started = parse_rfc3339_z(failed["startedTime"].as_str().unwrap()).unwrap();
    let inside = |a: &Value| {
        parse_rfc3339_z(a["creationTime"].as_str().unwrap_or("")).is_some_and(|t| {
            t.duration_since(started).unwrap_or_default() <= WINDOW
                && started.duration_since(t).unwrap_or_default() <= WINDOW
        })
    };
    let in_window: Vec<&str> = alerts
        .iter()
        .filter(|a| inside(a))
        .map(|a| a["extId"].as_str().unwrap())
        .collect();
    assert_eq!(
        in_window,
        vec![WARNING_IN_WINDOW],
        "alerts inside the window"
    );
    let critical = alerts
        .iter()
        .find(|a| a["extId"] == CRITICAL_OUTSIDE)
        .expect("the acknowledged CRITICAL");
    assert_eq!(critical["severity"], "CRITICAL");
    assert_eq!(critical["isAcknowledged"], true);
    assert!(!inside(critical), "the CRITICAL is outside the window");

    // Counts agree with the rows: a cluster's vmCount and a host's numberOfVms.
    for c in clusters {
        let id = c["extId"].as_str().unwrap();
        let expected = count(vms, |v| v["cluster"]["extId"] == id);
        assert_eq!(c["vmCount"], expected, "{}: vmCount", c["name"]);
    }
    for h in hosts {
        let id = h["extId"].as_str().unwrap();
        let expected = count(vms, |v| v["host"]["extId"] == id);
        assert_eq!(
            h["hypervisor"]["numberOfVms"], expected,
            "{}: hypervisor.numberOfVms",
            h["hostName"]
        );
    }
    let entering = count(hosts, |h| h["maintenanceState"] == "ENTERING_MAINTENANCE");
    assert_eq!(entering, 1, "one host entering maintenance");

    // The DR page's sampler: 5 protected resources keyed by VM id, 3/1/1.
    let protected = s
        .list("/dataprotection/v4.4/config/protected-resources")
        .expect("protected-resources");
    assert_eq!(protected.len(), 5);
    let status =
        |st: &'static str| move |r: &Value| r["replicationStates"][0]["replicationStatus"] == st;
    assert_eq!(count(protected, status("IN_SYNC")), 3);
    assert_eq!(count(protected, status("SYNCING")), 1);
    assert_eq!(count(protected, status("OUT_OF_SYNC")), 1);
    for p in protected {
        let id = p["extId"].as_str().unwrap();
        assert!(vms.iter().any(|v| v["extId"] == id), "{id} is a VM");
    }
}

/// Any string leaf under `v` equals `wanted`: how `can_i` reads `identities[].identityFilter`,
/// every string under it, so this reads it the same way.
fn any_leaf_equals(v: &Value, wanted: &str) -> bool {
    match v {
        Value::String(s) => s == wanted,
        Value::Array(a) => a.iter().any(|x| any_leaf_equals(x, wanted)),
        Value::Object(o) => o.values().any(|x| any_leaf_equals(x, wanted)),
        _ => false,
    }
}

/// Rule 2 for the ridge site: the `operator` account and its one policy, the header numbers,
/// the failed test-failover's evidence window, the sampler's rows, and its own identity.
fn ridge_pins(s: &Store) {
    let vms = rows_of(s, kind("vmm.ahv.config.Vm"));
    let hosts = rows_of(s, kind("clustermgmt.config.Host"));
    let clusters = rows_of(s, kind("clustermgmt.config.Cluster"));
    let alerts = rows_of(s, kind("monitoring.serviceability.Alert"));
    let tasks = rows_of(s, kind("prism.config.Task"));
    let users = rows_of(s, kind("iam.authn.User"));
    let groups = rows_of(s, kind("iam.authn.UserGroup"));
    let roles = rows_of(s, kind("iam.authz.Role"));
    let policies = rows_of(s, kind("iam.authz.AuthorizationPolicy"));
    let on = |r: &Value| r["powerState"] == "ON";
    let off = |r: &Value| r["powerState"] == "OFF";
    let open =
        |sev: &'static str| move |r: &Value| r["severity"] == sev && r["isResolved"] == false;

    // This site is pc-ridge: its domain manager is the id the harbor tree registered.
    let managers = rows_of(s, kind("prism.config.DomainManager"));
    assert_eq!(managers.len(), 1, "one domain manager");
    assert_eq!(
        managers[0]["extId"], RIDGE_DOMAIN_MANAGER,
        "pc-ridge's own id"
    );
    assert_eq!(managers[0]["config"]["name"], "pc-ridge");

    // `clusters 1 hosts 3 · vms 5 on/off 3/2 · alerts ⚠ 2 ✖ 1 · tasks ▶ 1 running`
    assert_eq!(clusters.len(), 1, "clusters");
    assert_eq!(hosts.len(), 3, "hosts");
    assert_eq!(vms.len(), 5, "vms");
    assert_eq!(count(vms, on), 3, "vms on");
    assert_eq!(count(vms, off), 2, "vms off");
    assert_eq!(count(alerts, open("CRITICAL")), 1, "critical unresolved");
    assert_eq!(count(alerts, open("WARNING")), 2, "warning unresolved");
    assert_eq!(
        count(tasks, |r| r["status"] == "RUNNING"),
        1,
        "running tasks"
    );
    assert_eq!(count(tasks, |r| r["status"] == "FAILED"), 1, "failed tasks");
    for vm in vms.iter().filter(|v| off(v)) {
        assert!(
            vm.get("host").is_none(),
            "{}: an OFF VM carries no host",
            vm["name"]
        );
    }
    for c in clusters {
        let id = c["extId"].as_str().unwrap();
        assert_eq!(
            c["vmCount"],
            count(vms, |v| v["cluster"]["extId"] == id),
            "{}: vmCount",
            c["name"]
        );
    }
    for h in hosts {
        let id = h["extId"].as_str().unwrap();
        assert_eq!(
            h["hypervisor"]["numberOfVms"],
            count(vms, |v| v["host"]["extId"] == id),
            "{}: hypervisor.numberOfVms",
            h["hostName"]
        );
    }

    // The account the demo logs in with here, and the one policy that names it.
    let operator = users
        .iter()
        .find(|u| u["username"] == "operator")
        .expect("operator is a ridge user");
    assert_eq!(operator["userType"], "LOCAL");
    let me = operator["extId"].as_str().unwrap();
    let mine: Vec<&Value> = policies
        .iter()
        .filter(|p| any_leaf_equals(&p["identities"], me))
        .collect();
    assert_eq!(mine.len(), 1, "exactly one policy names operator");
    let role = roles
        .iter()
        .find(|r| r["extId"] == mine[0]["role"])
        .expect("the policy's role is a ridge role");
    assert_eq!(role["displayName"], "Virtual Machine Operator");
    // No group carries an admin role: `can_i` counts a group-granted role for every user.
    for p in policies {
        let names_a_group = groups.iter().any(|g| {
            any_leaf_equals(&p["identities"], g["extId"].as_str().unwrap_or(""))
                || any_leaf_equals(&p["identities"], g["name"].as_str().unwrap_or(""))
        });
        if names_a_group {
            let display = roles
                .iter()
                .find(|r| r["extId"] == p["role"])
                .and_then(|r| r["displayName"].as_str())
                .unwrap_or("");
            assert!(
                !display.contains("Admin") && display != "Account Owner",
                "{display} is granted to a group by {}",
                p["extId"]
            );
        }
    }

    // The failed test-failover and the WARNING inside its window; the CRITICAL outside.
    let failed = tasks
        .iter()
        .find(|t| t["status"] == "FAILED")
        .expect("the failed test-failover task");
    assert!(
        failed["operationDescription"]
            .as_str()
            .unwrap_or("")
            .starts_with("Test failover rp-web-tier"),
        "the failed task is the test failover: {}",
        failed["operationDescription"]
    );
    assert!(
        failed["errorMessages"]
            .as_array()
            .is_some_and(|e| e.len() == 1)
    );
    assert!(failed["subSteps"].as_array().is_some_and(|e| e.len() == 2));
    let started = parse_rfc3339_z(failed["startedTime"].as_str().unwrap()).unwrap();
    let inside = |a: &Value| {
        parse_rfc3339_z(a["creationTime"].as_str().unwrap_or("")).is_some_and(|t| {
            t.duration_since(started).unwrap_or_default() <= WINDOW
                && started.duration_since(t).unwrap_or_default() <= WINDOW
        })
    };
    let in_window: Vec<&Value> = alerts.iter().filter(|a| inside(a)).collect();
    assert_eq!(in_window.len(), 1, "one alert inside the window");
    assert_eq!(in_window[0]["severity"], "WARNING");
    assert!(
        in_window[0]["title"]
            .as_str()
            .unwrap_or("")
            .contains("failover"),
        "the WARNING in the window is the failover one"
    );
    let critical = alerts
        .iter()
        .find(|a| a["severity"] == "CRITICAL")
        .expect("the unresolved CRITICAL");
    assert!(!inside(critical), "the CRITICAL is outside the window");

    // The sampler's rows: two protected VMs, IN_SYNC and SYNCING, both VMs of this site.
    let protected = s
        .list("/dataprotection/v4.4/config/protected-resources")
        .expect("protected-resources");
    assert_eq!(protected.len(), 2);
    let status =
        |st: &'static str| move |r: &Value| r["replicationStates"][0]["replicationStatus"] == st;
    assert_eq!(count(protected, status("IN_SYNC")), 1);
    assert_eq!(count(protected, status("SYNCING")), 1);
    for p in protected {
        let id = p["extId"].as_str().unwrap();
        assert!(vms.iter().any(|v| v["extId"] == id), "{id} is a VM");
    }

    // The inventory of Task 3, pinned: the storyboard sorts on these rows, and a file that
    // lost a row to an edit is found here rather than in a frame.
    for (path, want) in [
        ("/prism/v4.4/config/tasks", 5),
        ("/prism/v4.4/config/categories", 4),
        ("/clustermgmt/v4.3/config/disks", 9),
        ("/clustermgmt/v4.3/config/host-nics", 6),
        ("/clustermgmt/v4.3/config/storage-containers", 2),
        ("/vmm/v4.3/ahv/config/vm-recovery-points", 2),
        ("/datapolicies/v4.3/config/protection-policies", 3),
        ("/datapolicies/v4.3/config/recovery-plans", 2),
        ("/dataprotection/v4.4/config/recovery-plan-jobs", 2),
        ("/dataprotection/v4.4/config/recovery-points", 2),
        ("/networking/v4.4/config/subnets", 2),
        ("/microseg/v4.3/config/service-groups", 2),
        ("/monitoring/v4.3/serviceability/alerts", 3),
        ("/monitoring/v4.3/serviceability/events", 4),
        ("/monitoring/v4.3/serviceability/audits", 4),
        ("/iam/v4.0/authn/users", 3),
        ("/iam/v4.0/authz/roles", 4),
        ("/iam/v4.0/authz/authorization-policies", 2),
        ("/lifecycle/v4.3/resources/entities", 3),
        ("/lifecycle/v4.3/resources/lcm-histories", 2),
        // Two, not the inventory's one: the coverage gate asks two rows of every non-singleton
        // kind on some site, and `objects` is served on the ridge alone.
        ("/objects/v4.1/config/object-stores", 2),
        (
            "/objects/v4.1/config/object-stores/0b7ec000-0b02-4000-8000-000000000001/buckets",
            3,
        ),
    ] {
        assert_eq!(s.list(path).map_or(0, <[Value]>::len), want, "{path}");
    }
    let buckets = s
        .list("/objects/v4.1/config/object-stores/0b7ec000-0b02-4000-8000-000000000001/buckets")
        .unwrap();
    assert_eq!(
        count(buckets, |b| b["isObjectLockEnabled"] == true),
        1,
        "one locked bucket"
    );
}

#[test]
fn the_story_pins_hold() {
    for site in SITES {
        let s = at_t0(site);
        let domains = rows_of(&s, kind("multidomain.config.RegisteredDomain"));
        assert_eq!(
            domains[0]["extId"], site.sibling_domain_manager,
            "{}: registered-domains[0] is the other site's domain manager",
            site.name
        );
        for (key, value, at) in walk(&s) {
            if (key.ends_with("Time") || key.ends_with("Timestamp"))
                && key != "expirationTime"
                && let Value::String(t) = &value
            {
                assert!(
                    t.as_str() < CUTOFF,
                    "{}: {at} = {t} is not before {CUTOFF}",
                    site.name
                );
            }
        }
        match site.name {
            "demo" => harbor_pins(&s),
            "demo-dr" => ridge_pins(&s),
            other => panic!("no pins for site {other}"),
        }
    }
}

// ── cross-reference rule 3: the coverage gate ─────────────────────────────────────────────

/// Kinds that are one row by nature: a single-entity endpoint, and one registered peer.
const SINGLETONS: &[&str] = &[
    "prism.config.DomainManager",
    "multidomain.config.RegisteredDomain",
];

/// Every kind the sidebar's curated groups open and every kind a page pane lists.
fn gated_kinds() -> Vec<&'static Kind> {
    let mut ids: Vec<&str> = NAV[..CURATED_GROUPS]
        .iter()
        .flat_map(|g| g.items.iter())
        .filter_map(|i| match i.target {
            NavTarget::Kind(id) => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 74, "the sidebar's curated kinds");
    ids.extend(
        PAGES
            .iter()
            .flat_map(|p| p.panes.iter().map(|pane| pane.kind)),
    );
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter().map(kind).collect()
}

#[test]
fn every_nav_and_pane_kind_has_rows() {
    let kinds = gated_kinds();
    let stores: Vec<(&Site, Store)> = SITES.iter().map(|site| (site, at_t0(site))).collect();
    let mut short = Vec::new();
    for k in &kinds {
        let mut most = 0;
        for (site, s) in &stores {
            if site.absent.contains(&k.namespace) || site.empty.contains(&k.id) {
                continue;
            }
            let n = rows_of(s, k).len();
            if n == 0 {
                short.push(format!(
                    "{}: {} has no rows at {}",
                    site.name, k.id, k.list_path
                ));
            }
            most = most.max(n);
        }
        let serving: Vec<&str> = stores
            .iter()
            .filter(|(site, _)| !site.absent.contains(&k.namespace) && !site.empty.contains(&k.id))
            .map(|(site, _)| site.name)
            .collect();
        if !serving.is_empty() && most < 2 && !SINGLETONS.contains(&k.id) {
            short.push(format!(
                "{}: {} has {most} row(s) on every site that serves it; a sort needs two",
                serving.join("+"),
                k.id
            ));
        }
    }
    assert!(
        short.is_empty(),
        "{} gap(s):\n{}",
        short.len(),
        short.join("\n")
    );
}

// ── the load-time rules ───────────────────────────────────────────────────────────────────

#[test]
fn derived_children_exist_for_every_vm_and_cluster() {
    for site in SITES {
        let s = at_t0(site);
        let len = |p: &str| s.list(p).map(<[Value]>::len);
        let vms_path = nutsh_catalog::with_version(
            kind("vmm.ahv.config.Vm").list_path,
            &s.version_of("vmm").expect("vmm fixtures"),
        );
        for vm in s.list(&vms_path).unwrap() {
            let id = vm["extId"].as_str().unwrap();
            for (field, child) in [
                ("disks", "disks"),
                ("nics", "nics"),
                ("cdRoms", "cd-roms"),
                ("gpus", "gpus"),
                ("serialPorts", "serial-ports"),
            ] {
                let inline = vm[field].as_array().map_or(0, Vec::len);
                assert_eq!(
                    len(&format!("{vms_path}/{id}/{child}")),
                    Some(inline),
                    "{}: {}/{child}",
                    site.name,
                    vm["name"]
                );
            }
        }
        let clusters_path = nutsh_catalog::with_version(
            kind("clustermgmt.config.Cluster").list_path,
            &s.version_of("clustermgmt").expect("clustermgmt fixtures"),
        );
        let hosts = rows_of(&s, kind("clustermgmt.config.Host"));
        let nics = rows_of(&s, kind("clustermgmt.config.HostNic"));
        let mut hosts_seen = 0;
        for c in s.list(&clusters_path).unwrap() {
            let cid = c["extId"].as_str().unwrap();
            let members = s
                .list(&format!("{clusters_path}/{cid}/hosts"))
                .unwrap_or_else(|| panic!("{}: {cid}/hosts", site.name));
            assert_eq!(
                members.len(),
                count(hosts, |h| h["cluster"]["uuid"] == cid),
                "{}: hosts of {}",
                site.name,
                c["name"]
            );
            assert_eq!(
                members.len() as u64,
                c["nodes"]["numberOfNodes"].as_u64().unwrap()
            );
            hosts_seen += members.len();
            for h in members {
                let hid = h["extId"].as_str().unwrap();
                assert_eq!(
                    len(&format!("{clusters_path}/{cid}/hosts/{hid}/host-nics")),
                    Some(count(nics, |n| n["nodeUuid"] == hid)),
                    "{}: host-nics of {}",
                    site.name,
                    h["hostName"]
                );
                assert!(
                    count(nics, |n| n["nodeUuid"] == hid) >= 1,
                    "{}: {} has a NIC",
                    site.name,
                    h["hostName"]
                );
            }
        }
        assert_eq!(
            hosts_seen,
            hosts.len(),
            "{}: every host is under its cluster",
            site.name
        );
        let tasks_path = nutsh_catalog::with_version(
            kind("prism.config.Task").list_path,
            &s.version_of("prism").expect("prism fixtures"),
        );
        for t in s.list(&tasks_path).unwrap() {
            let id = t["extId"].as_str().unwrap();
            assert_eq!(
                len(&format!("{tasks_path}/{id}/affected-entities")),
                Some(t["entitiesAffected"].as_array().map_or(0, Vec::len)),
                "{}: affected-entities of {}",
                site.name,
                t["operationDescription"]
            );
        }
    }
}

#[test]
fn rebase_shifts_every_timestamp_and_usecs() {
    for site in SITES {
        let base = at_t0(site);
        let later = store(site.table, t0() + Duration::from_secs(3600)).unwrap();
        assert_eq!(base.paths(), later.paths(), "{}: same lists", site.name);
        let (a, b) = (walk(&base), walk(&later));
        assert_eq!(a.len(), b.len(), "{}: same scalars", site.name);
        let mut moved = 0usize;
        for ((ka, va, at), (kb, vb, _)) in a.iter().zip(&b) {
            assert_eq!(ka, kb, "{}: {at}", site.name);
            if (ka.ends_with("Time") || ka.ends_with("Timestamp"))
                && let Value::String(x) = va
            {
                assert!(
                    parse_rfc3339_z(x).is_some(),
                    "{}: {at} = {x} is not YYYY-MM-DDTHH:MM:SSZ and would not be shifted",
                    site.name
                );
            }
            match (va, vb) {
                (Value::String(x), Value::String(y)) if parse_rfc3339_z(x).is_some() => {
                    let tx = parse_rfc3339_z(x).unwrap();
                    let ty = parse_rfc3339_z(y)
                        .unwrap_or_else(|| panic!("{}: {at} rebased to {y}", site.name));
                    assert_eq!(
                        ty.duration_since(tx).ok(),
                        Some(Duration::from_secs(3600)),
                        "{}: {at}",
                        site.name
                    );
                    moved += 1;
                }
                (Value::Number(x), Value::Number(y)) if ka.ends_with("Usecs") => {
                    assert_eq!(
                        y.as_i64().unwrap() - x.as_i64().unwrap(),
                        3_600_000_000,
                        "{}: {at}",
                        site.name
                    );
                    moved += 1;
                }
                _ => assert_eq!(va, vb, "{}: {at} changed under rebase", site.name),
            }
        }
        assert!(moved > 100, "{}: {moved} values moved", site.name);
        // Backwards: a clock behind T0 moves everything the other way.
        let earlier = walk(&store(site.table, t0() - Duration::from_secs(86_400)).unwrap());
        let (at, x, y) = a
            .iter()
            .zip(&earlier)
            .find_map(|((_, va, at), (_, ve, _))| match (va, ve) {
                (Value::String(x), Value::String(y)) if parse_rfc3339_z(x).is_some() => {
                    Some((at, x, y))
                }
                _ => None,
            })
            .expect("at least one timestamp");
        assert_eq!(
            parse_rfc3339_z(x)
                .unwrap()
                .duration_since(parse_rfc3339_z(y).unwrap())
                .ok(),
            Some(Duration::from_secs(86_400)),
            "{}: {at}",
            site.name
        );
    }
}
