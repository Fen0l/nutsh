//! The estate `nutsh demo` boots, embedded in this library so a released binary needs no
//! fixture directory on disk.
//!
//! One `(path, text)` pair per fixture file, read with `include_str!` at compile time, so a
//! table entry with no file fails to compile; the equality test in `tests/demo.rs` catches the
//! other direction, a file on disk the table does not name. [`store`] turns a table into a
//! [`Store`] in three steps: [`Store::from_files`], then a shift of every canonical RFC 3339
//! string and every `*Usecs` integer by `now - T0` so ages read right on the day the demo runs,
//! then the child lists the catalog reaches by drilling, derived from the parent rows rather
//! than kept as files that could drift from them.
//!
//! `derive_nested` lives here and not on `Store` on purpose: `fixtures/` commits its child
//! lists as files (`fixtures/clustermgmt/.../clusters/<id>/hosts.json`) and a derivation on
//! `Store` would silently overwrite them.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::{Store, rfc3339};

/// The instant every timestamp in `fixtures-demo/` is written around: `2026-09-05T10:00:00Z`
/// as Unix seconds. Under `--snapshot` the demo runs at exactly this instant, so every age a
/// frame shows is the one written in the file.
pub const T0_UNIX_SECS: u64 = 1_788_602_400;

/// [`T0_UNIX_SECS`] as a `SystemTime`. A function, not a constant: `SystemTime` has no
/// `const` constructor past `UNIX_EPOCH`.
pub fn t0() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(T0_UNIX_SECS)
}

/// One row of a site table: the fixture's path relative to the site root, which is also its
/// key in the store once `.json` is removed, and its text.
macro_rules! site {
    ($root:literal, $path:literal) => {
        (
            $path,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fixtures-demo/",
                $root,
                "/",
                $path
            )),
        )
    };
}

/// The `pc-harbor` site (context `demo`). Every file under `fixtures-demo/demo/`, in the order
/// of the inventory; `tests/demo.rs` holds this equal to the tree on disk.
pub const DEMO: &[(&str, &str)] = &[
    site!("demo", "prism/v4.4/config/domain-managers.json"),
    site!("demo", "prism/v4.4/config/tasks.json"),
    site!("demo", "prism/v4.4/config/categories.json"),
    site!(
        "demo",
        "prism/v4.4/config/tasks/ZXJnb24=:7a5c0000-0a01-4000-8000-000000000001/jobs.json"
    ),
    site!("demo", "clustermgmt/v4.3/config/clusters.json"),
    site!("demo", "clustermgmt/v4.3/config/hosts.json"),
    site!("demo", "clustermgmt/v4.3/config/storage-containers.json"),
    site!("demo", "clustermgmt/v4.3/config/disks.json"),
    site!("demo", "clustermgmt/v4.3/config/host-nics.json"),
    site!("demo", "clustermgmt/v4.3/config/external-storages.json"),
    site!(
        "demo",
        "clustermgmt/v4.3/ahv/config/physical-gpu-profiles.json"
    ),
    site!(
        "demo",
        "clustermgmt/v4.3/ahv/config/virtual-gpu-profiles.json"
    ),
    site!("demo", "clustermgmt/v4.3/ahv/config/pcie-devices.json"),
    site!(
        "demo",
        "clustermgmt/v4.3/config/clusters/01000000-0a01-4000-8000-000000000001/cvms.json"
    ),
    site!(
        "demo",
        "clustermgmt/v4.3/config/clusters/01000000-0a01-4000-8000-000000000001/rackable-units.json"
    ),
    site!(
        "demo",
        "clustermgmt/v4.3/config/clusters/01000000-0a01-4000-8000-000000000002/cvms.json"
    ),
    site!(
        "demo",
        "clustermgmt/v4.3/config/clusters/01000000-0a01-4000-8000-000000000002/rackable-units.json"
    ),
    site!("demo", "vmm/v4.3/ahv/config/vms.json"),
    site!("demo", "vmm/v4.3/ahv/config/vm-profiles.json"),
    site!(
        "demo",
        "vmm/v4.3/ahv/config/vm-guest-customization-profiles.json"
    ),
    site!("demo", "vmm/v4.3/ahv/config/vm-recovery-points.json"),
    site!("demo", "vmm/v4.3/esxi/config/vms.json"),
    site!("demo", "vmm/v4.3/content/images.json"),
    site!("demo", "vmm/v4.3/content/templates.json"),
    site!("demo", "vmm/v4.3/content/ovas.json"),
    site!(
        "demo",
        "vmm/v4.3/content/templates/2a000000-0a01-4000-8000-000000000001/versions.json"
    ),
    site!(
        "demo",
        "vmm/v4.3/ahv/policies/vm-host-affinity-policies.json"
    ),
    site!(
        "demo",
        "vmm/v4.3/ahv/policies/vm-anti-affinity-policies.json"
    ),
    site!("demo", "vmm/v4.3/ahv/policies/vm-startup-policies.json"),
    site!("demo", "vmm/v4.3/ahv/policies/guest-reboot-policies.json"),
    site!("demo", "vmm/v4.3/images/config/placement-policies.json"),
    site!("demo", "vmm/v4.3/images/config/rate-limit-policies.json"),
    site!("demo", "volumes/v4.3/config/volume-groups.json"),
    site!(
        "demo",
        "volumes/v4.3/config/volume-groups/0a000000-0a01-4000-8000-000000000001/disks.json"
    ),
    site!(
        "demo",
        "volumes/v4.3/config/volume-groups/0a000000-0a01-4000-8000-000000000001/vm-attachments.json"
    ),
    site!("demo", "datapolicies/v4.3/config/protection-policies.json"),
    site!("demo", "datapolicies/v4.3/config/recovery-plans.json"),
    site!("demo", "datapolicies/v4.3/config/storage-policies.json"),
    site!("demo", "datapolicies/v4.3/config/entity-sync-policies.json"),
    site!(
        "demo",
        "datapolicies/v4.3/config/recovery-plans/12000000-0a01-4000-8000-000000000001/stages.json"
    ),
    site!("demo", "dataprotection/v4.4/config/recovery-plan-jobs.json"),
    site!("demo", "dataprotection/v4.4/config/recovery-points.json"),
    site!(
        "demo",
        "dataprotection/v4.4/config/recovery-point-stores.json"
    ),
    site!(
        "demo",
        "dataprotection/v4.4/config/protected-resources.json"
    ),
    site!(
        "demo",
        "dataprotection/v4.4/config/recovery-plan-jobs/13000000-0a01-4000-8000-000000000002/validation-errors.json"
    ),
    site!("demo", "networking/v4.4/config/subnets.json"),
    site!("demo", "networking/v4.4/config/vpcs.json"),
    site!("demo", "networking/v4.4/config/floating-ips.json"),
    site!("demo", "networking/v4.4/config/virtual-switches.json"),
    site!("demo", "networking/v4.4/config/layer2-stretches.json"),
    site!("demo", "networking/v4.4/config/gateways.json"),
    site!("demo", "networking/v4.4/config/vpn-connections.json"),
    site!("demo", "networking/v4.4/config/bgp-sessions.json"),
    site!("demo", "networking/v4.4/config/routing-policies.json"),
    site!("demo", "networking/v4.4/config/traffic-mirrors.json"),
    site!("demo", "networking/v4.4/config/load-balancer-sessions.json"),
    site!("demo", "networking/v4.4/config/nic-profiles.json"),
    site!(
        "demo",
        "networking/v4.4/config/subnets/08000000-0a01-4000-8000-000000000001/reserved-ips.json"
    ),
    site!("demo", "microseg/v4.3/config/policies.json"),
    site!("demo", "microseg/v4.3/config/address-groups.json"),
    site!("demo", "microseg/v4.3/config/service-groups.json"),
    site!(
        "demo",
        "microseg/v4.3/config/policies/19000000-0a01-4000-8000-000000000002/rules.json"
    ),
    site!("demo", "monitoring/v4.3/serviceability/alerts.json"),
    site!("demo", "monitoring/v4.3/serviceability/events.json"),
    site!("demo", "monitoring/v4.3/serviceability/audits.json"),
    site!(
        "demo",
        "monitoring/v4.3/serviceability/alerts/user-defined-policies.json"
    ),
    site!("demo", "opsmgmt/v4.0/config/reports.json"),
    site!("demo", "security/v4.1/management/approval-policies.json"),
    site!("demo", "security/v4.1/config/key-management-servers.json"),
    site!("demo", "iam/v4.0/authn/users.json"),
    site!("demo", "iam/v4.0/authn/user-groups.json"),
    site!("demo", "iam/v4.0/authn/directory-services.json"),
    site!("demo", "iam/v4.0/authn/saml-identity-providers.json"),
    site!("demo", "iam/v4.0/authz/roles.json"),
    site!("demo", "iam/v4.0/authz/authorization-policies.json"),
    site!("demo", "lifecycle/v4.3/resources/entities.json"),
    site!("demo", "lifecycle/v4.3/resources/bundles.json"),
    site!("demo", "lifecycle/v4.3/resources/lcm-histories.json"),
    site!("demo", "licensing/v4.4/config/licenses.json"),
    site!("demo", "licensing/v4.4/config/entitlements.json"),
    site!("demo", "licensing/v4.4/config/compliances.json"),
    site!("demo", "multidomain/v4.3/config/registered-domains.json"),
    site!("demo", "files/v4.0/config/file-servers.json"),
    site!("demo", "files/v4.0/config/replication-policies.json"),
    site!("demo", "files/v4.0/operations/replication-jobs.json"),
    site!("demo", "files/v4.0/config/unified-namespaces.json"),
    site!(
        "demo",
        "files/v4.0/config/file-servers/44000000-0a01-4000-8000-000000000001/mount-targets.json"
    ),
];

/// The ridge site, `demo-dr`: pc-ridge, one cluster, five VMs, the `operator` account, an
/// object store, and no licensing, files, opsmgmt or security namespace at all. Same rule as
/// [`DEMO`]: the file on disk and this table are one thing, `tests/demo.rs` holds them equal.
pub const DEMO_DR: &[(&str, &str)] = &[
    site!("demo-dr", "prism/v4.4/config/domain-managers.json"),
    site!("demo-dr", "prism/v4.4/config/tasks.json"),
    site!(
        "demo-dr",
        "prism/v4.4/config/tasks/ZXJnb24=:7a5c0000-0b02-4000-8000-000000000002/jobs.json"
    ),
    site!("demo-dr", "prism/v4.4/config/categories.json"),
    site!("demo-dr", "clustermgmt/v4.3/config/clusters.json"),
    site!("demo-dr", "clustermgmt/v4.3/config/hosts.json"),
    site!(
        "demo-dr",
        "clustermgmt/v4.3/config/clusters/c1000000-0b02-4000-8000-000000000001/cvms.json"
    ),
    site!(
        "demo-dr",
        "clustermgmt/v4.3/config/clusters/c1000000-0b02-4000-8000-000000000001/rackable-units.json"
    ),
    site!("demo-dr", "clustermgmt/v4.3/config/storage-containers.json"),
    site!("demo-dr", "clustermgmt/v4.3/config/disks.json"),
    site!("demo-dr", "clustermgmt/v4.3/config/host-nics.json"),
    site!("demo-dr", "clustermgmt/v4.3/config/external-storages.json"),
    site!("demo-dr", "clustermgmt/v4.3/ahv/config/pcie-devices.json"),
    site!("demo-dr", "vmm/v4.3/ahv/config/vms.json"),
    site!("demo-dr", "vmm/v4.3/ahv/config/vm-profiles.json"),
    site!(
        "demo-dr",
        "vmm/v4.3/ahv/config/vm-guest-customization-profiles.json"
    ),
    site!("demo-dr", "vmm/v4.3/ahv/config/vm-recovery-points.json"),
    site!("demo-dr", "vmm/v4.3/content/images.json"),
    site!("demo-dr", "vmm/v4.3/content/templates.json"),
    site!(
        "demo-dr",
        "vmm/v4.3/ahv/policies/vm-host-affinity-policies.json"
    ),
    site!(
        "demo-dr",
        "vmm/v4.3/ahv/policies/vm-anti-affinity-policies.json"
    ),
    site!("demo-dr", "vmm/v4.3/ahv/policies/vm-startup-policies.json"),
    site!(
        "demo-dr",
        "vmm/v4.3/ahv/policies/guest-reboot-policies.json"
    ),
    site!("demo-dr", "vmm/v4.3/images/config/placement-policies.json"),
    site!("demo-dr", "vmm/v4.3/images/config/rate-limit-policies.json"),
    site!("demo-dr", "volumes/v4.3/config/volume-groups.json"),
    site!(
        "demo-dr",
        "volumes/v4.3/config/volume-groups/0c900000-0b02-4000-8000-000000000001/disks.json"
    ),
    site!(
        "demo-dr",
        "volumes/v4.3/config/volume-groups/0c900000-0b02-4000-8000-000000000001/vm-attachments.json"
    ),
    site!(
        "demo-dr",
        "datapolicies/v4.3/config/protection-policies.json"
    ),
    site!("demo-dr", "datapolicies/v4.3/config/recovery-plans.json"),
    site!("demo-dr", "datapolicies/v4.3/config/storage-policies.json"),
    site!(
        "demo-dr",
        "datapolicies/v4.3/config/entity-sync-policies.json"
    ),
    site!(
        "demo-dr",
        "dataprotection/v4.4/config/recovery-plan-jobs.json"
    ),
    site!(
        "demo-dr",
        "dataprotection/v4.4/config/recovery-plan-jobs/10b00000-0b02-4000-8000-000000000001/validation-errors.json"
    ),
    site!("demo-dr", "dataprotection/v4.4/config/recovery-points.json"),
    site!(
        "demo-dr",
        "dataprotection/v4.4/config/recovery-point-stores.json"
    ),
    site!(
        "demo-dr",
        "dataprotection/v4.4/config/protected-resources.json"
    ),
    site!("demo-dr", "networking/v4.4/config/subnets.json"),
    site!("demo-dr", "networking/v4.4/config/vpcs.json"),
    site!("demo-dr", "networking/v4.4/config/floating-ips.json"),
    site!("demo-dr", "networking/v4.4/config/virtual-switches.json"),
    site!("demo-dr", "networking/v4.4/config/layer2-stretches.json"),
    site!("demo-dr", "networking/v4.4/config/gateways.json"),
    site!("demo-dr", "networking/v4.4/config/vpn-connections.json"),
    site!("demo-dr", "networking/v4.4/config/bgp-sessions.json"),
    site!("demo-dr", "networking/v4.4/config/routing-policies.json"),
    site!("demo-dr", "networking/v4.4/config/traffic-mirrors.json"),
    site!(
        "demo-dr",
        "networking/v4.4/config/load-balancer-sessions.json"
    ),
    site!("demo-dr", "microseg/v4.3/config/policies.json"),
    site!("demo-dr", "microseg/v4.3/config/address-groups.json"),
    site!("demo-dr", "microseg/v4.3/config/service-groups.json"),
    site!("demo-dr", "monitoring/v4.3/serviceability/alerts.json"),
    site!("demo-dr", "monitoring/v4.3/serviceability/events.json"),
    site!("demo-dr", "monitoring/v4.3/serviceability/audits.json"),
    site!(
        "demo-dr",
        "monitoring/v4.3/serviceability/alerts/user-defined-policies.json"
    ),
    site!("demo-dr", "iam/v4.0/authn/users.json"),
    site!("demo-dr", "iam/v4.0/authn/user-groups.json"),
    site!("demo-dr", "iam/v4.0/authn/directory-services.json"),
    site!("demo-dr", "iam/v4.0/authz/roles.json"),
    site!("demo-dr", "iam/v4.0/authz/authorization-policies.json"),
    site!("demo-dr", "lifecycle/v4.3/resources/entities.json"),
    site!("demo-dr", "lifecycle/v4.3/resources/bundles.json"),
    site!("demo-dr", "lifecycle/v4.3/resources/lcm-histories.json"),
    site!("demo-dr", "multidomain/v4.3/config/registered-domains.json"),
    site!("demo-dr", "objects/v4.1/config/object-stores.json"),
    site!(
        "demo-dr",
        "objects/v4.1/config/object-stores/0b7ec000-0b02-4000-8000-000000000001/buckets.json"
    ),
];

/// A site table as the mock serves it at `now`: the files, every timestamp moved by
/// `now - T0`, and the derived child lists. The `Err` is a file that does not parse, which the
/// equality test in `tests/demo.rs` makes impossible on a committed tree.
pub fn store(table: &[(&str, &str)], now: SystemTime) -> serde_json::Result<Store> {
    let mut store = Store::from_files(table)?;
    let delta = delta_secs(now);
    if delta != 0 {
        for &(path, _) in table {
            let key = format!("/{}", path.strip_suffix(".json").unwrap_or(path));
            let mut rows = rows(&store, &key);
            for row in &mut rows {
                rebase(row, "", delta);
            }
            store.insert(&key, rows);
        }
    }
    derive_nested(&mut store);
    Ok(store)
}

/// `now - T0` in whole seconds, negative when the clock is behind `T0`.
fn delta_secs(now: SystemTime) -> i64 {
    match now.duration_since(t0()) {
        Ok(ahead) => i64::try_from(ahead.as_secs()).unwrap_or(i64::MAX / 4),
        Err(behind) => -i64::try_from(behind.duration().as_secs()).unwrap_or(i64::MAX / 4),
    }
}

fn shift(t: SystemTime, delta: i64) -> SystemTime {
    if delta >= 0 {
        t + Duration::from_secs(delta.unsigned_abs())
    } else {
        t - Duration::from_secs(delta.unsigned_abs())
    }
}

/// Every canonical RFC 3339 string moved by `delta` seconds and every integer under a key
/// ending in `Usecs` moved by `delta` microseconds; nothing else changes. `key` is the name
/// the value sits under, inherited by array items.
fn rebase(value: &mut Value, key: &str, delta: i64) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                rebase(v, k, delta);
            }
        }
        Value::Array(items) => {
            for v in items {
                rebase(v, key, delta);
            }
        }
        Value::String(s) => {
            if let Some(t) = parse_rfc3339_z(s) {
                *s = rfc3339(shift(t, delta));
            }
        }
        Value::Number(n) if key.ends_with("Usecs") => {
            if let Some(i) = n.as_i64() {
                *n = serde_json::Number::from(i + delta * 1_000_000);
            }
        }
        _ => {}
    }
}

/// The inverse of [`rfc3339`], on its output and nothing else. Public so a test can read a
/// fixture's instant back without a second date library.
///
/// `YYYY-MM-DDTHH:MM:SSZ` and only that: what `rfc3339` writes and what every fixture here
/// is written in. An offset, a fraction or a bare date is `None` and is not shifted.
pub fn parse_rfc3339_z(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        if !b[from..to].iter().all(u8::is_ascii_digit) {
            return None;
        }
        s[from..to].parse().ok()
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Days from civil, the inverse of the civil-from-days arithmetic in `clock.rs`.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

/// The child lists the catalog reaches by drilling, built from the parent rows: a VM's
/// `disks`/`nics`/`cdRoms`/`gpus`/`serialPorts` arrays, a cluster's hosts by `cluster.uuid`,
/// a host's NICs by `nodeUuid`, a task's `entitiesAffected`. A parent without the array gets
/// an empty child list, so drilling shows an empty table rather than a 404 on a served path.
fn derive_nested(store: &mut Store) {
    if let Some(vms) = path_of(store, "vmm.ahv.config.Vm") {
        for vm in rows(store, &vms) {
            let Some(id) = vm["extId"].as_str() else {
                continue;
            };
            for (field, child) in [
                ("disks", "disks"),
                ("nics", "nics"),
                ("cdRoms", "cd-roms"),
                ("gpus", "gpus"),
                ("serialPorts", "serial-ports"),
            ] {
                let items = vm[field].as_array().cloned().unwrap_or_default();
                store.insert(&format!("{vms}/{id}/{child}"), items);
            }
        }
    }
    if let (Some(clusters), Some(hosts)) = (
        path_of(store, "clustermgmt.config.Cluster"),
        path_of(store, "clustermgmt.config.Host"),
    ) {
        let all_hosts = rows(store, &hosts);
        let all_nics = path_of(store, "clustermgmt.config.HostNic")
            .map(|p| rows(store, &p))
            .unwrap_or_default();
        for cluster in rows(store, &clusters) {
            let Some(cid) = cluster["extId"].as_str() else {
                continue;
            };
            let members: Vec<Value> = all_hosts
                .iter()
                .filter(|h| h["cluster"]["uuid"].as_str() == Some(cid))
                .cloned()
                .collect();
            for host in &members {
                let Some(hid) = host["extId"].as_str() else {
                    continue;
                };
                let nics: Vec<Value> = all_nics
                    .iter()
                    .filter(|n| n["nodeUuid"].as_str() == Some(hid))
                    .cloned()
                    .collect();
                store.insert(&format!("{clusters}/{cid}/hosts/{hid}/host-nics"), nics);
            }
            store.insert(&format!("{clusters}/{cid}/hosts"), members);
        }
    }
    if let Some(tasks) = path_of(store, "prism.config.Task") {
        for task in rows(store, &tasks) {
            let Some(id) = task["extId"].as_str() else {
                continue;
            };
            let affected = task["entitiesAffected"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            store.insert(&format!("{tasks}/{id}/affected-entities"), affected);
        }
    }
}

/// A kind's list path at the version the store's files carry, so the derived lists sit
/// beside the files whatever version the catalog moves to; `None` when the store has no file
/// under that namespace.
fn path_of(store: &Store, kind_id: &str) -> Option<String> {
    let kind = nutsh_catalog::kind(kind_id)?;
    let version = store.version_of(kind.namespace)?;
    Some(nutsh_catalog::with_version(kind.list_path, &version))
}

/// An owned copy of a list, or nothing: the callers edit the store while they iterate.
fn rows(store: &Store, path: &str) -> Vec<Value> {
    store.list(path).map(<[Value]>::to_vec).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn t0_is_the_fixtures_reference_instant() {
        assert_eq!(rfc3339(t0()), "2026-09-05T10:00:00Z");
        assert_eq!(parse_rfc3339_z("2026-09-05T10:00:00Z"), Some(t0()));
    }

    #[test]
    fn the_parser_is_the_inverse_of_the_formatter() {
        for s in [
            "1970-01-01T00:00:00Z",
            "2026-02-28T23:59:59Z",
            "2026-07-07T10:00:00Z",
            "2026-09-05T09:51:00Z",
            "2027-01-01T00:00:00Z",
        ] {
            assert_eq!(rfc3339(parse_rfc3339_z(s).unwrap()), s, "{s}");
        }
        // Not canonical: an offset, a fraction, a date, a version. None of these is shifted.
        for s in [
            "2026-09-05T10:00:00+02:00",
            "2026-09-05T10:00:00.520017Z",
            "2026-12-10",
            "10.3.1.2",
            "",
        ] {
            assert_eq!(parse_rfc3339_z(s), None, "{s}");
        }
    }

    #[test]
    fn rebase_moves_canonical_strings_and_usecs_and_nothing_else() {
        let mut row = json!({
            "createTime": "2026-09-05T09:00:00Z",
            "bootTimeUsecs": 1_788_598_800_000_000_i64,
            "nested": {"lastUpdatedTime": "2026-09-05T09:30:00Z", "expiryDate": "2026-12-10"},
            "list": ["2026-09-05T09:45:00Z", 7],
            "count": 3,
            "name": "prd-web-01"
        });
        rebase(&mut row, "", 3600);
        assert_eq!(row["createTime"], "2026-09-05T10:00:00Z");
        assert_eq!(row["bootTimeUsecs"], 1_788_602_400_000_000_i64);
        assert_eq!(row["nested"]["lastUpdatedTime"], "2026-09-05T10:30:00Z");
        assert_eq!(row["nested"]["expiryDate"], "2026-12-10");
        assert_eq!(row["list"][0], "2026-09-05T10:45:00Z");
        assert_eq!(row["list"][1], 7);
        assert_eq!(
            row["count"], 3,
            "an integer under a key not ending in Usecs is left alone"
        );
        assert_eq!(row["name"], "prd-web-01");
        // Backwards too: a clock behind T0 is a negative delta.
        rebase(&mut row, "", -7200);
        assert_eq!(row["createTime"], "2026-09-05T08:00:00Z");
        assert_eq!(row["bootTimeUsecs"], 1_788_595_200_000_000_i64);
    }

    #[test]
    fn derive_nested_builds_the_drill_targets_from_the_parents() {
        let mut store = Store::default();
        store.insert(
            "/vmm/v4.3/ahv/config/vms",
            vec![json!({
                "extId": "vm-1",
                "disks": [{"extId": "d-1"}, {"extId": "d-2"}],
                "nics": [{"extId": "n-1"}],
                "cdRoms": [{"extId": "c-1"}]
            })],
        );
        store.insert(
            "/clustermgmt/v4.3/config/clusters",
            vec![json!({"extId": "c-a"}), json!({"extId": "c-b"})],
        );
        store.insert(
            "/clustermgmt/v4.3/config/hosts",
            vec![
                json!({"extId": "h-1", "cluster": {"uuid": "c-a"}}),
                json!({"extId": "h-2", "cluster": {"uuid": "c-b"}}),
            ],
        );
        store.insert(
            "/clustermgmt/v4.3/config/host-nics",
            vec![
                json!({"extId": "nic-1", "nodeUuid": "h-1"}),
                json!({"extId": "nic-2", "nodeUuid": "h-2"}),
                json!({"extId": "nic-3", "nodeUuid": "h-2"}),
            ],
        );
        store.insert(
            "/prism/v4.4/config/tasks",
            vec![json!({
                "extId": "ZXJnb24=:t-1",
                "entitiesAffected": [{"extId": "vm-1", "name": "vm", "rel": "vmm:ahv:config:vm"}]
            })],
        );
        derive_nested(&mut store);
        let len = |p: &str| store.list(p).map(<[Value]>::len);
        assert_eq!(len("/vmm/v4.3/ahv/config/vms/vm-1/disks"), Some(2));
        assert_eq!(len("/vmm/v4.3/ahv/config/vms/vm-1/nics"), Some(1));
        assert_eq!(len("/vmm/v4.3/ahv/config/vms/vm-1/cd-roms"), Some(1));
        assert_eq!(
            len("/vmm/v4.3/ahv/config/vms/vm-1/gpus"),
            Some(0),
            "a VM without the array still gets an empty child list"
        );
        assert_eq!(len("/vmm/v4.3/ahv/config/vms/vm-1/serial-ports"), Some(0));
        assert_eq!(len("/clustermgmt/v4.3/config/clusters/c-a/hosts"), Some(1));
        assert_eq!(len("/clustermgmt/v4.3/config/clusters/c-b/hosts"), Some(1));
        assert_eq!(
            len("/clustermgmt/v4.3/config/clusters/c-b/hosts/h-2/host-nics"),
            Some(2)
        );
        assert_eq!(
            len("/clustermgmt/v4.3/config/clusters/c-a/hosts/h-1/host-nics"),
            Some(1)
        );
        assert_eq!(
            len("/prism/v4.4/config/tasks/ZXJnb24=:t-1/affected-entities"),
            Some(1)
        );
        assert_eq!(
            store
                .list("/clustermgmt/v4.3/config/clusters/c-a/hosts")
                .unwrap()[0]["extId"],
            "h-1"
        );
    }
}
