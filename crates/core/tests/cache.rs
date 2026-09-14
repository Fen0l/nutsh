//! The cache directory: what survives a round trip, what invalidates it, and what it refuses
//! to write. No Prism Central, no terminal, no clock.

use std::collections::BTreeMap;
use std::path::Path;

use nutsh_core::cache::{
    self, ClusterRecord, FORMAT, Identity, Meta, Snapshot, StatsRecord, TableFile, TableSnapshot,
};
use serde_json::{Value, json};

fn identity() -> Identity {
    Identity {
        host: "pc.lab.example".into(),
        port: 9440,
        username: "admin".into(),
        domain_manager: Some("dm-1".into()),
        pc_version: Some("pc.7.6".into()),
    }
}

fn vms() -> &'static nutsh_catalog::Kind {
    nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs")
}

/// The `$select` this run would send for `id`, which is what the writer records against a
/// table's rows. A kind the catalog does not know narrows nothing.
fn select_of(id: &str) -> Option<String> {
    nutsh_catalog::kind(id)?.select.map(str::to_string)
}

fn vm(id: &str, name: &str) -> Value {
    json!({"extId": id, "name": name, "powerState": "ON"})
}

fn snapshot(rows: Vec<Value>) -> Snapshot {
    Snapshot {
        identity: identity(),
        pins: BTreeMap::from([("vmm".to_string(), "v4.3".to_string())]),
        namespaces: Vec::new(),
        cluster: Some(ClusterRecord {
            ext_id: "0006".into(),
            name: "prod-01".into(),
        }),
        names: vec![("0006".to_string(), "prod-01".to_string())],
        stats: Some(StatsRecord {
            clusters: Some(3),
            hosts: Some(7),
            vms: Some(100),
            vms_on: Some(88),
            alerts_critical: Some(2),
            alerts_warning: Some(9),
            tasks_running: Some(1),
            at: 1_000,
        }),
        sample: None,
        can_i: None,
        tables: vec![TableSnapshot {
            kind: "vmm.ahv.config.Vm".into(),
            parents: Vec::new(),
            filter: None,
            // What the scheduler sends on the cycle that fills this table, which is what the
            // writer records: these rows are that narrowing and not a whole document.
            select: vms().select.map(str::to_string),
            total: Some(100),
            at: 1_000,
            rows,
        }],
    }
}

fn meta(dir: &Path) -> Meta {
    serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap()
}

/// Everything the first frame paints comes back: rows in order, totals, names, counters, pins,
/// the resolved cluster.
#[test]
fn a_round_trip_keeps_the_rows_the_names_the_counters_and_the_pins() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().join("lab");
    let snap = snapshot(vec![vm("a", "web-01"), vm("b", "web-02")]);
    cache::write(&dir, snap, 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    assert_eq!(r.written, 1_000);
    assert_eq!(r.pins.get("vmm").map(String::as_str), Some("v4.3"));
    assert_eq!(r.cluster.as_ref().unwrap().name, "prod-01");
    assert_eq!(r.names, vec![("0006".to_string(), "prod-01".to_string())]);
    assert_eq!(r.stats.unwrap().vms_on, Some(88));
    assert_eq!(r.tables.len(), 1);
    let t = &r.tables[0];
    assert_eq!(t.kind.id, "vmm.ahv.config.Vm");
    assert_eq!(t.total, Some(100));
    assert_eq!(t.filter, None);
    let ids: Vec<&str> = t
        .rows
        .iter()
        .map(|v| v["extId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a", "b"], "server order survives");
}

/// Every file 0600 inside a 0700 directory, the reasoning `config.toml` applies, reusing its
/// code. Not a credential - the cache holds none - but an inventory of your infrastructure.
#[cfg(unix)]
#[test]
fn every_file_is_private_inside_a_private_directory() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        assert_eq!(mode(&path), 0o600, "{}", path.display());
    }
}

/// A newer format is left entirely alone: no read, **no write, no eviction**. An older nutsh
/// must not clobber a newer one's directory, and the startup sweep does not count it against
/// the eight either - nine directories go in and nine come out.
#[test]
fn a_newer_format_is_not_read_not_written_and_not_evicted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    let mut m = meta(&dir);
    m.format = FORMAT + 1;
    std::fs::write(dir.join("meta.json"), serde_json::to_string(&m).unwrap()).unwrap();
    let before = listing(&dir);
    assert!(cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).is_none());
    cache::write(&dir, snapshot(vec![vm("z", "zz")]), 2_000).unwrap();
    assert_eq!(listing(&dir), before, "byte-identical afterwards");

    // Eight ordinary siblings, all written after it: on `written` alone the newer-format one is
    // the least recent and would be the ninth evicted. It is skipped instead.
    for i in 0..8 {
        cache::write(
            &tmp.path().join(format!("ctx{i}")),
            snapshot(vec![vm("a", "web-01")]),
            5_000 + i as u64,
        )
        .unwrap();
    }
    cache::sweep(tmp.path()).unwrap();
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 9);
    assert_eq!(listing(&dir), before, "and it is still byte-identical");
}

fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// Corruption is not news: nothing loads, every file the cache wrote goes, and no flash line
/// reports it.
#[test]
fn a_corrupt_meta_loads_nothing_and_removes_what_the_cache_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    std::fs::write(dir.join("meta.json"), "{ not json").unwrap();
    assert!(cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).is_none());
    assert!(cache::peek(&dir).is_none());
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "nothing of the cache's is left"
    );
}

/// A corrupt *table* file costs that table and nothing else, and the file is unlinked.
#[test]
fn a_corrupt_table_file_costs_that_table_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let mut snap = snapshot(vec![vm("a", "web-01")]);
    snap.tables.push(TableSnapshot {
        kind: "clustermgmt.config.Cluster".into(),
        parents: Vec::new(),
        filter: None,
        select: select_of("clustermgmt.config.Cluster"),
        total: Some(3),
        at: 1_000,
        rows: vec![json!({"extId": "c1", "name": "prod-01"})],
    });
    cache::write(&dir, snap, 1_000).unwrap();
    let vm_file = meta(&dir)
        .tables
        .iter()
        .find(|t| t.kind == "vmm.ahv.config.Vm")
        .unwrap()
        .file
        .clone();
    std::fs::write(dir.join(&vm_file), "{ not json").unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("the rest loads");
    assert_eq!(r.tables.len(), 1);
    assert_eq!(r.tables[0].kind.id, "clustermgmt.config.Cluster");
    assert!(!dir.join(&vm_file).exists(), "the bad file is unlinked");
}

/// The header a `t-*.json` carries is what makes a collision merge nothing and an orphan stay
/// an orphan: every field of it is re-checked against the record that named the file, and a
/// mismatch on any one costs that table and unlinks that file while the rest of the directory
/// restores. A file from another process's session is the orphan case; a `kind`, `parents` or
/// `filter` that is not the record's is the hash-collision case.
#[test]
fn a_table_file_that_disowns_its_record_is_dropped_and_unlinked() {
    type Mutation = (&'static str, fn(&mut TableFile));
    let mutations: [Mutation; 5] = [
        ("format", |f| f.format = FORMAT + 1),
        ("session", |f| f.session = "0123456789abcdef".into()),
        ("kind", |f| f.kind = "clustermgmt.config.Cluster".into()),
        ("parents", |f| f.parents = vec!["someone-else".into()]),
        // A filter a page really declares, so that this case is the header check rather than
        // the resolution below it.
        ("filter", |f| f.filter = Some("isResolved eq false".into())),
    ];
    for (label, mutate) in mutations {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lab");
        let mut snap = snapshot(vec![vm("a", "web-01")]);
        snap.tables.push(TableSnapshot {
            kind: "clustermgmt.config.Cluster".into(),
            parents: Vec::new(),
            filter: None,
            select: select_of("clustermgmt.config.Cluster"),
            total: Some(3),
            at: 1_000,
            rows: vec![json!({"extId": "c1", "name": "prod-01"})],
        });
        cache::write(&dir, snap, 1_000).unwrap();
        let vm_file = meta(&dir)
            .tables
            .iter()
            .find(|t| t.kind == "vmm.ahv.config.Vm")
            .unwrap()
            .file
            .clone();
        let path = dir.join(&vm_file);
        let mut file: TableFile =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).expect("a table file");
        mutate(&mut file);
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("the rest loads");
        assert_eq!(r.tables.len(), 1, "{label}");
        assert_eq!(r.tables[0].kind.id, "clustermgmt.config.Cluster", "{label}");
        assert!(!path.exists(), "{label}: the disowned file is unlinked");
    }
}

/// A file `meta.tables` names but that is not there is an ordinary outcome, not corruption:
/// another nutsh's sweep can unlink it between our read of the meta and our open of it.
#[test]
fn a_missing_table_file_drops_its_record_and_keeps_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    let file = meta(&dir).tables[0].file.clone();
    std::fs::remove_file(dir.join(&file)).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("still a restore");
    assert!(r.tables.is_empty());
    assert_eq!(r.pins.get("vmm").map(String::as_str), Some("v4.3"));
}

/// A different account, a different host, eight days, and a bumped app version: each is the
/// whole directory. The first three are a different `target` or a later `now`; the fourth is a
/// rewritten `meta.json`, because the version is the writer's own and no argument carries it.
#[test]
fn a_changed_target_or_an_old_write_uses_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    for (label, target, now) in [
        (
            "another user",
            Identity {
                username: "someone".into(),
                ..identity()
            },
            1_060u64,
        ),
        (
            "another host",
            Identity {
                host: "pc.other".into(),
                ..identity()
            },
            1_060,
        ),
        ("eight days on", identity(), 1_000 + cache::MAX_AGE + 1),
    ] {
        let dir = tmp.path().join(label.replace(' ', "-"));
        cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
        assert!(
            cache::read(&dir, &target, now, cache::MAX_AGE).is_none(),
            "{label}"
        );
        assert!(
            !dir.join("meta.json").exists(),
            "{label}: and the inventory is removed"
        );
        assert!(cache::peek(&dir).is_none(), "{label}: nothing left to read");
    }

    // The fourth row, which no `Identity` can express: the same target, in date, written by a
    // different build. A shape this nutsh does not know how to read is not worth guessing at.
    let dir = tmp.path().join("another-build");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    let mut m = meta(&dir);
    m.app_version = format!("{}-and-a-bit", m.app_version);
    std::fs::write(dir.join("meta.json"), serde_json::to_string(&m).unwrap()).unwrap();
    assert!(
        cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).is_none(),
        "app_version"
    );
    assert!(
        !dir.join("meta.json").exists(),
        "app_version: and the inventory is removed"
    );
}

/// The palette's `history.json` shares the directory and is not the cache's to throw away: an
/// upgrade discards the inventory once, and the user's typed lines are not part of it.
/// `nutsh cache clear` is the other thing, and still removes the directory whole.
#[test]
fn discarding_a_cache_keeps_the_palette_history_beside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    std::fs::write(
        dir.join("history.json"),
        r#"{"version":1,"entries":["vm"]}"#,
    )
    .unwrap();
    let mut m = meta(&dir);
    m.app_version = format!("{}-and-a-bit", m.app_version);
    std::fs::write(dir.join("meta.json"), serde_json::to_string(&m).unwrap()).unwrap();

    assert!(cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).is_none());
    assert!(!dir.join("meta.json").exists(), "the inventory went");
    assert_eq!(
        std::fs::read_to_string(dir.join("history.json")).unwrap(),
        r#"{"version":1,"entries":["vm"]}"#,
        "and the history did not"
    );

    cache::remove(&dir).unwrap();
    assert!(!dir.exists(), "clear removes every byte, history included");
}

/// A `select` that no longer matches what this run would send is a projection the columns have
/// outgrown: that table goes, the rest stays.
#[test]
fn a_stale_select_drops_that_table_and_keeps_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let mut snap = snapshot(vec![vm("a", "web-01")]);
    snap.tables[0].select = Some("extId,name".into());
    cache::write(&dir, snap, 1_000).unwrap();
    let file = meta(&dir).tables[0].file.clone();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    assert!(
        r.tables.is_empty(),
        "a narrowing this run does not send is not this run's rows"
    );
    assert!(!dir.join(&file).exists(), "and the file is unlinked");
    assert_eq!(r.pins.get("vmm").map(String::as_str), Some("v4.3"));
}

/// The narrowing a table's rows carry is part of what identifies them, and the guard that
/// checks it has never once fired.
///
/// The writer labelled every table `None` - a whole document - while the scheduler has been
/// sending `$select` on the cycle that fills one all along. So the comparison was `None`
/// against `None`, and rows with no `disks` and no `bootConfig` came back next start as if
/// they were complete. Both ends have to move together: a writer that labels honestly against
/// a reader that still expects `None` would discard every table on every start.
#[test]
fn rows_narrowed_by_select_are_labelled_with_the_narrowing_they_carry() {
    assert!(
        vms().select.is_some(),
        "this test is about a kind the scheduler narrows"
    );
    let tmp = tempfile::tempdir().unwrap();

    // What this run writes, read back by this run.
    let dir = tmp.path().join("now");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    assert_eq!(r.tables.len(), 1, "its own rows come back");
    assert_eq!(r.tables[0].rows.len(), 1);

    // And what the version that mislabelled them left behind: narrowed rows filed as whole
    // documents. One discard on upgrade, which is the right answer for a row that is not what
    // its label says.
    let stale = tmp.path().join("before");
    let mut snap = snapshot(vec![vm("a", "web-01")]);
    snap.tables[0].select = None;
    cache::write(&stale, snap, 1_000).unwrap();
    let file = meta(&stale).tables[0].file.clone();
    let r = cache::read(&stale, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    assert!(
        r.tables.is_empty(),
        "a table labelled as a whole document is not restored over a narrowed one"
    );
    assert!(!stale.join(&file).exists(), "and the file is unlinked");
}

/// A kind the catalog no longer knows, and a row that is not an object or has no ext id.
#[test]
fn an_unknown_kind_and_an_unusable_row_are_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let mut snap = snapshot(vec![
        vm("a", "web-01"),
        json!("not an object"),
        json!({"name": "no ext id"}),
    ]);
    snap.tables.push(TableSnapshot {
        kind: "gone.config.Thing".into(),
        parents: Vec::new(),
        filter: None,
        select: None,
        total: None,
        at: 1_000,
        rows: vec![json!({"extId": "x"})],
    });
    cache::write(&dir, snap, 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    assert_eq!(r.tables.len(), 1, "the unknown kind is gone");
    let ids: Vec<&str> = r.tables[0]
        .rows
        .iter()
        .map(|v| v["extId"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(ids, ["a"], "and so are the two unusable rows");
}

/// The caps: rows per table, bytes per directory, and names. The per-table cap has its own
/// test below, because reaching it means skipping a table rather than trimming one.
#[test]
fn the_caps_hold_and_eviction_takes_the_oldest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let many: Vec<Value> = (0..3_000).map(|i| vm(&format!("id{i}"), "row")).collect();
    let mut snap = snapshot(many);
    snap.names = (0..25_000)
        .map(|i| (format!("id{i}"), format!("name{i}")))
        .collect();
    cache::write(&dir, snap, 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    assert_eq!(r.tables[0].rows.len(), cache::MAX_ROWS_PER_TABLE);
    assert_eq!(r.tables[0].total, Some(100), "the server's total is kept");
    assert_eq!(r.names.len(), cache::MAX_NAMES);

    // Twenty tables of a megabyte each: the directory ends under its cap with the newest kept.
    let padding = "x".repeat(1_000);
    let mut big = snapshot(Vec::new());
    big.tables.clear();
    for i in 0..20 {
        big.tables.push(TableSnapshot {
            kind: "vmm.ahv.config.Vm".into(),
            parents: vec![format!("parent{i}")],
            filter: None,
            select: select_of("vmm.ahv.config.Vm"),
            total: None,
            at: 1_000 + i as u64,
            rows: (0..1_000)
                .map(|r| json!({"extId": format!("{i}-{r}"), "name": padding}))
                .collect(),
        });
    }
    let dir2 = tmp.path().join("big");
    cache::write(&dir2, big, 2_000).unwrap();
    // The table files alone: `meta.json` is written after the eviction loop and is outside the
    // cap by design, bounded by MAX_NAMES instead. See MAX_DIR_BYTES.
    let bytes: u64 = table_bytes(&dir2);
    assert!(bytes <= cache::MAX_DIR_BYTES, "{bytes} bytes");
    let kept = meta(&dir2);
    assert!(!kept.tables.is_empty());
    let oldest = kept.tables.iter().map(|t| t.at).min().unwrap();
    assert!(oldest > 1_000, "the oldest were the ones evicted");
}

/// Every `t-*.json` in a directory, by name.
fn table_files(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("t-") && n.ends_with(".json"))
        .collect();
    out.sort();
    out
}

fn table_bytes(dir: &Path) -> u64 {
    table_files(dir)
        .iter()
        .map(|n| std::fs::metadata(dir.join(n)).unwrap().len())
        .sum()
}

/// A table that serializes over the per-table cap is skipped whole, not trimmed further - and
/// the file a smaller earlier write left for it is unlinked by the final sweep, so a stale
/// version of that table cannot survive as an orphan the next meta never names.
#[test]
fn a_table_over_the_per_table_cap_is_skipped_and_its_stale_file_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();
    let file = meta(&dir).tables[0].file.clone();
    assert!(dir.join(&file).exists(), "the small write is cached");

    // Same kind, same parents, same filter: the same file name, over 2 MB of rows.
    let padding = "x".repeat(10_000);
    let mut snap = snapshot(
        (0..300)
            .map(|i| json!({"extId": format!("id{i}"), "name": padding}))
            .collect(),
    );
    snap.tables[0].at = 2_000;
    cache::write(&dir, snap, 2_000).unwrap();
    let after = meta(&dir);
    assert!(
        after.tables.is_empty(),
        "no record names it: {:?}",
        after.tables
    );
    assert_eq!(
        table_files(&dir),
        Vec::<String>::new(),
        "and no file either"
    );
    assert!(
        cache::read(&dir, &identity(), 2_060, cache::MAX_AGE)
            .unwrap()
            .tables
            .is_empty()
    );
}

/// A ninth context directory evicts the least recently written.
#[test]
fn a_ninth_context_directory_evicts_the_least_recently_written() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..9 {
        let dir = tmp.path().join(format!("ctx{i}"));
        cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000 + i as u64).unwrap();
    }
    cache::sweep(tmp.path()).unwrap();
    let left: Vec<String> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left.len(), cache::MAX_CONTEXT_DIRS);
    assert!(!left.contains(&"ctx0".to_string()), "{left:?}");
    assert!(left.contains(&"ctx8".to_string()), "{left:?}");
}

/// Every key path of a value, dotted, arrays flattened.
fn key_paths(v: &Value, prefix: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                out.push(path.clone());
                key_paths(v, &path, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                key_paths(item, prefix, out);
            }
        }
        _ => {}
    }
}

fn lab(relative: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mockpc/fixtures-lab")
        .join(relative);
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

/// What the recording proves is really there: `authorizedPublicKeyList`, three times in
/// the clusters fixture, with twenty `key` entries under it. The committed values are the
/// recorder's placeholder rather than the twenty real SSH public keys the recording serves, so what
/// bites here is the **name** rule - the whole list goes, at whatever depth it sits, before the
/// shape of any value is consulted. The value rule is pinned separately, on a PEM blob, in
/// `a_category_key_survives_and_a_pem_blob_does_not`, and so is `guestCustomization`, which no
/// recorded fixture carries: this walk holds the corpus to the same standard, but only the
/// synthetic row can prove that branch deletes.
#[test]
fn no_key_material_and_no_guest_customization_survive_a_write() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let mut snap = snapshot(lab("vmm/v4.3/ahv/config/vms.json"));
    snap.tables.push(TableSnapshot {
        kind: "clustermgmt.config.Cluster".into(),
        parents: Vec::new(),
        filter: None,
        select: select_of("clustermgmt.config.Cluster"),
        total: None,
        at: 1_000,
        rows: lab("clustermgmt/v4.3/config/clusters.json"),
    });
    cache::write(&dir, snap, 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    let mut paths = Vec::new();
    for t in &r.tables {
        for row in &t.rows {
            key_paths(row, "", &mut paths);
        }
    }
    assert!(!paths.is_empty(), "the fixtures are not empty");
    for p in &paths {
        let last = p.rsplit('.').next().unwrap_or(p).to_ascii_lowercase();
        assert_ne!(last, "guestcustomization", "{p}");
        assert!(
            !last.contains("publickey") && !last.contains("privatekey"),
            "{p}"
        );
    }
}

/// The bare-`script` marker: a Task row still carries its
/// `operationDescription`, which the recorder redacts on all 100 rows of the committed
/// fixture today.
#[test]
fn a_cached_task_row_still_carries_its_operation_description() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let mut snap = snapshot(Vec::new());
    snap.tables[0] = TableSnapshot {
        kind: "prism.config.Task".into(),
        parents: Vec::new(),
        filter: None,
        select: select_of("prism.config.Task"),
        total: None,
        at: 1_000,
        rows: lab("prism/v4.4/config/tasks.json"),
    };
    cache::write(&dir, snap, 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    let with_description = r.tables[0]
        .rows
        .iter()
        .filter(|row| row.get("operationDescription").is_some())
        .count();
    assert!(
        with_description > 0,
        "every operationDescription was scrubbed: the script marker is unanchored again"
    );
}

/// A bare `key` holding half a category pair survives; one holding PEM text does not. And the
/// structure and the key order of everything else are untouched - `serde_json`'s
/// `preserve_order` is already on in this crate.
///
/// This row is also the only pin the `guestCustomization` branch has: no recorded fixture
/// carries that subtree, and it is the one thing §5.1 names that a per-field walk cannot make
/// safe - the payload is base64, so the encoded root password inside it is not a key any rule
/// can see. It is deleted whole, by name, and its non-secret sibling is not.
#[test]
fn a_category_key_survives_and_a_pem_blob_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let row = json!({
        "extId": "cat-1",
        "key": "environment",
        "value": "prod",
        "nested": {"key": "-----BEGIN CERTIFICATE-----AAAA", "keep": 1},
        "config": {
            "guestCustomization": {"config": {"cloudInitScript": "I2Nsb3VkLWNvbmZpZwo="}},
            "guestcustomization": {"config": {"sysprepScript": "PHVuYXR0ZW5kPgo="}},
            "bootConfig": {"bootDevice": "DISK"}
        },
        "order": {"z": 1, "a": 2}
    });
    let mut snap = snapshot(vec![row]);
    snap.tables[0].kind = "prism.config.Category".into();
    snap.tables[0].select = select_of("prism.config.Category");
    cache::write(&dir, snap, 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    let back = &r.tables[0].rows[0];
    assert_eq!(back["key"], json!("environment"));
    assert_eq!(back["value"], json!("prod"));
    assert!(back["nested"].get("key").is_none(), "{back}");
    assert_eq!(back["nested"]["keep"], json!(1));
    assert!(
        back["config"].get("guestCustomization").is_none(),
        "the base64 subtree reached the disk: {back}"
    );
    assert!(
        back["config"].get("guestcustomization").is_none(),
        "and the name is matched case-insensitively: {back}"
    );
    assert_eq!(
        back["config"]["bootConfig"]["bootDevice"],
        json!("DISK"),
        "its non-secret sibling is untouched"
    );
    let order: Vec<&String> = back["order"].as_object().unwrap().keys().collect();
    assert_eq!(order, ["z", "a"], "key order survives the round trip");
}

/// One assertion per keep-list name and per `…Description` column: none of them may be
/// scrubbed out of a cached row, because no upstream refusal will ever remove them and the
/// blanking would be permanent.
#[test]
fn the_keep_list_and_every_description_survive_a_write() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    let kept = [
        "hasPrivateKey",
        "shouldValidateAdCredential",
        "isForceResetPasswordEnabled",
        "claimTokenExtId",
        "accessKeyName",
        "apiCredentialStatus",
        "credentialIssuer",
        "authorizationPolicyType",
        "description",
        "entityDescription",
        "hostDescription",
        "operationDescription",
        "stepDescription",
        "templateDescription",
        "threatDescription",
        "versionDescription",
    ];
    let mut row = serde_json::Map::new();
    row.insert("extId".into(), json!("x"));
    for k in kept {
        row.insert(k.to_string(), json!("kept"));
    }
    cache::write(&dir, snapshot(vec![Value::Object(row)]), 1_000).unwrap();
    let r = cache::read(&dir, &identity(), 1_060, cache::MAX_AGE).expect("a restore");
    let back = &r.tables[0].rows[0];
    for k in kept {
        assert_eq!(back.get(k), Some(&json!("kept")), "{k} was scrubbed");
    }
}

/// `peek` is what `nutsh cache info` reports through, so it reports and never judges: a
/// directory `read` would discard - an older format, another build's `app_version` - comes
/// back described, with its files still there. `info` is the command that explains an empty
/// first frame; it must never be a second command that deletes one.
#[test]
fn peek_describes_a_directory_read_would_refuse_and_removes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lab");
    cache::write(&dir, snapshot(vec![vm("a", "web-01")]), 1_000).unwrap();

    let p = cache::peek(&dir).expect("a meta");
    assert_eq!(p.format, FORMAT);
    assert_eq!(p.app_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(p.written, 1_000);
    assert_eq!(p.identity, identity());
    assert_eq!(p.pins, 1);
    assert_eq!(p.tables.len(), 1);
    assert_eq!(p.tables[0].kind, "vmm.ahv.config.Vm");
    assert_eq!(p.tables[0].rows, 1);
    assert!(p.tables[0].bytes > 0);

    let mut m = meta(&dir);
    m.format = FORMAT + 1;
    m.app_version = "9.9.9".into();
    std::fs::write(dir.join("meta.json"), serde_json::to_string(&m).unwrap()).unwrap();
    let p = cache::peek(&dir).expect("a meta this build would refuse is still a meta");
    assert_eq!(p.format, FORMAT + 1);
    assert_eq!(p.app_version, "9.9.9");
    assert!(dir.join("meta.json").exists(), "peek removed nothing");

    std::fs::write(dir.join("meta.json"), "not json").unwrap();
    assert!(
        cache::peek(&dir).is_none(),
        "unreadable is None, not a panic"
    );
    assert!(dir.exists(), "and still not a delete");
}
