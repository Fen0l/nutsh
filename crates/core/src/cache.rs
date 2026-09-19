//! What the last run knew, on disk: one directory per context, `meta.json` plus one
//! `t-<hash>.json` per table.
//!
//! It is not an offline mode and not an HTTP response cache. It exists so that the first frame
//! of the second run is not empty, and so that negotiation, the cluster resolution and the
//! permission walk are done once rather than once per start. Every row it holds is scrubbed of
//! secret-shaped keys through the rule `nutsh_catalog::secret` shares with the fixture recorder
//! and the generator, and `ext_id`/`name` are re-derived through the *current* catalog on load,
//! so a `name_path` curated since the write is honoured instead of a stale derivation being
//! resurrected.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use nutsh_catalog::{Kind, secret};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The on-disk format. A **newer** one is left entirely alone - not read, not written, not
/// evicted - so an older nutsh cannot clobber a newer one's directory.
pub const FORMAT: u32 = 1;

/// Whole-directory expiry.
pub const MAX_AGE: u64 = 7 * 24 * 60 * 60;
/// How fresh a restored namespace status has to be to be adopted. Applied by the restore path,
/// not by [`read`], which hands the records back with their `at` untouched.
pub const NAMESPACE_TTL: u64 = 24 * 60 * 60;
/// The permission walk's result rots sooner than a rename does. Applied by the restore path,
/// not by [`read`].
pub const CAN_I_TTL: u64 = 60 * 60;
/// The first 2 000 rows in server order. A truncated restore is honest for free: the header
/// already prints `shown/total`, and `total` is the server's.
pub const MAX_ROWS_PER_TABLE: usize = 2_000;
/// A table that serializes larger is skipped, not truncated further.
pub const MAX_TABLE_BYTES: u64 = 2 * 1024 * 1024;
/// Over this, whole table files are evicted oldest-`at` first until under. It bounds the
/// `t-*.json` files alone: `meta.json` is written after the eviction loop and is outside the
/// cap, which is what [`MAX_NAMES`] bounds instead.
pub const MAX_DIR_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_NAMES: usize = 20_000;
/// On start, the least recently written directories above this are removed.
pub const MAX_CONTEXT_DIRS: usize = 8;
/// The longest slug-clean prefix a directory name keeps before the hash.
const MAX_SLUG: usize = 40;

/// Which Prism Central, under which account, at which version - checked *after* connecting for
/// the last two, because the first frame must paint before the network answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// The domain manager's `extId`. `None` when it did not answer.
    #[serde(default)]
    pub domain_manager: Option<String>,
    /// e.g. `pc.7.6`. `None` when it did not answer.
    #[serde(default)]
    pub pc_version: Option<String>,
}

impl Identity {
    /// The half that is known before a single request: the target the command line resolved.
    pub fn same_target(&self, other: &Identity) -> bool {
        self.host == other.host && self.port == other.port && self.username == other.username
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceRecord {
    pub name: String,
    pub version: String,
    pub pinned: Option<String>,
    pub ok: bool,
    pub detail: String,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterRecord {
    pub ext_id: String,
    pub name: String,
}

/// The seven counters as the header draws them. A mirror rather than `serde` on
/// `stats::Stats`: the cache owns its own file format, and `stats.rs` is another task's file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsRecord {
    pub clusters: Option<u64>,
    pub hosts: Option<u64>,
    pub vms: Option<u64>,
    pub vms_on: Option<u64>,
    pub alerts_critical: Option<u64>,
    pub alerts_warning: Option<u64>,
    pub tasks_running: Option<u64>,
    pub at: u64,
}

/// The Disaster Recovery sampler's last result, likewise mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleRecord {
    pub policies: Option<u64>,
    pub plans: Option<u64>,
    pub recovery_points: Option<u64>,
    pub jobs: Option<u64>,
    pub sampled: u64,
    pub in_sync: u64,
    pub syncing: u64,
    pub out_of_sync: u64,
    pub available: bool,
    pub at: u64,
}

/// The **roles**, never the resolved action set: `CanIndex::with_roles` computes `can()` from
/// the roles *plus* the catalog's policies, so a persisted verdict would rot the first time a
/// policy or an action id changed. Restoring means calling `with_roles` again, which is cheap
/// and always current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanIRecord {
    pub username: String,
    pub at: u64,
    pub roles: Vec<String>,
}

/// One table's entry in `meta.json`, one per `t-*.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRecord {
    pub file: String,
    pub kind: String,
    pub parents: Vec<String>,
    pub filter: Option<String>,
    /// The `$select` this table's rows were fetched with: what this run would have to send to
    /// match them.
    pub select: Option<String>,
    pub rows: usize,
    pub total: Option<u64>,
    pub bytes: u64,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub format: u32,
    /// Unix seconds. No `time` feature change: an age is a subtraction, and `nutsh cache info`
    /// is where a human reads one.
    pub written: u64,
    /// 16 hex, this process's. Ties table files to this meta, so an orphan another process
    /// left behind is recognised rather than merged.
    pub session: String,
    pub app_version: String,
    pub identity: Identity,
    pub pins: BTreeMap<String, String>,
    pub namespaces: Vec<NamespaceRecord>,
    pub cluster: Option<ClusterRecord>,
    pub names: Vec<(String, String)>,
    pub stats: Option<StatsRecord>,
    pub sample: Option<SampleRecord>,
    pub can_i: Option<CanIRecord>,
    pub tables: Vec<TableRecord>,
}

/// A table file. Its `kind`/`parents`/`filter`/`select`/`total`/`at` are re-checked here and
/// never trusted from `meta.json`, so a hash collision is caught by comparing the fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableFile {
    pub format: u32,
    pub session: String,
    pub kind: String,
    pub parents: Vec<String>,
    pub filter: Option<String>,
    pub select: Option<String>,
    pub total: Option<u64>,
    pub at: u64,
    /// Scrubbed raw entities, in server order.
    pub rows: Vec<Value>,
}

/// Seconds since the epoch: the convention every `now: u64` in this module takes, and the one
/// the restore path's TTLs are measured against. `0` for a clock before the epoch, which makes
/// every record read as ancient rather than as fresh.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The directory key for `name`, or `None` for a session that must run cacheless.
///
/// `None` for `""`, `.` and `..` - an outright rejection, never a hashed key. `valid_name`
/// permits `.`, so `..` is a legal context name; hashing it would silently name a directory
/// the user cannot connect to what they typed, and not hashing it would escape the tree. A
/// `None` key means one `tracing::warn!`, nothing read, nothing written, and a program
/// otherwise unchanged.
pub fn slug(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    // Every kept character is ASCII and every other one becomes `_`, so the result is ASCII
    // and can be sliced by bytes below.
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean == name && clean.len() <= MAX_SLUG {
        return Some(clean);
    }
    let head = &clean[..clean.len().min(MAX_SLUG)];
    Some(format!("{head}-{:016x}", fnv1a64(name)))
}

/// FNV-1a-64, eight lines rather than a hash crate. Used for the directory suffix and the
/// table file name, neither of which is a security boundary: a collision is caught by
/// comparing the fields the name was built from.
///
/// `xtask::record::hash64` is the same function and stays a separate copy on purpose: it names
/// committed fixture files, so the two must be free to be judged - and if ever changed - apart.
/// Sharing them would tie a cache file name nobody reads to a corpus diff everybody reviews.
pub(crate) fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// `t-` plus 16 hex of the three fields `TableKey`'s `Hash` and `Eq` are written over.
///
/// The `\0` separators and the parent *count* together are what keep `["a", "b"]` and
/// `["a\0b"]` apart: separators alone cannot, because both join to the same bytes.
pub(crate) fn table_file_name(kind: &str, parents: &[String], filter: Option<&str>) -> String {
    let key = format!(
        "{kind}\u{0}{}\u{0}{}\u{0}{}",
        parents.len(),
        parents.join("\u{0}"),
        filter.unwrap_or("")
    );
    format!("t-{:016x}.json", fnv1a64(&key))
}

/// What one write is handed: cloned raw rows, names, counters and pins, built on the UI thread
/// and serialized on a blocking one. The clone costs roughly the serialized size (750 KB for a
/// 100-VM table) once a minute, which is much cheaper than a dropped frame - and it is the only
/// one, because [`write()`] takes this by value and moves through it.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub identity: Identity,
    pub pins: BTreeMap<String, String>,
    /// Written positives-first; a `false` is never adopted on read and is not worth the bytes.
    pub namespaces: Vec<NamespaceRecord>,
    pub cluster: Option<ClusterRecord>,
    pub names: Vec<(String, String)>,
    pub stats: Option<StatsRecord>,
    pub sample: Option<SampleRecord>,
    pub can_i: Option<CanIRecord>,
    pub tables: Vec<TableSnapshot>,
}

#[derive(Debug, Clone)]
pub struct TableSnapshot {
    pub kind: String,
    pub parents: Vec<String>,
    pub filter: Option<String>,
    pub select: Option<String>,
    pub total: Option<u64>,
    pub at: u64,
    pub rows: Vec<Value>,
}

/// What a directory gave back. Nothing here is a `Store` yet: the restore path turns the
/// tables into rows and the pins and namespaces into a `Client`'s routing.
#[derive(Debug, Clone)]
pub struct Restored {
    pub dir: PathBuf,
    pub written: u64,
    /// The identity **as stored**, including the domain manager and the PC version, which are
    /// compared after connecting because they are not known before.
    pub identity: Identity,
    pub pins: BTreeMap<String, String>,
    /// Positives only, as written. Each is adopted by the restore path only while
    /// `now - at <= `[`NAMESPACE_TTL`]; [`read`] does not apply that itself.
    pub namespaces: Vec<NamespaceRecord>,
    pub cluster: Option<ClusterRecord>,
    pub names: Vec<(String, String)>,
    pub stats: Option<StatsRecord>,
    pub sample: Option<SampleRecord>,
    /// The roles, adopted by the restore path only while `now - at <= `[`CAN_I_TTL`] and the
    /// username still matches; [`read`] does not apply that itself.
    pub can_i: Option<CanIRecord>,
    pub tables: Vec<RestoredTable>,
}

#[derive(Debug, Clone)]
pub struct RestoredTable {
    pub kind: &'static Kind,
    pub parents: Vec<String>,
    /// Resolved back to the `&'static str` some page declares, because that is what
    /// `TableKey::filter` is. A filter no page declares any more is a table no view can open.
    pub filter: Option<&'static str>,
    pub rows: Vec<Value>,
    pub total: Option<u64>,
    pub at: u64,
}

/// The `$select` **this run** would send for `kind`, which is what a recorded projection has to
/// match. `Subscription::list` sends `kind.select` on the cycle that fills a table, so a
/// restored table's rows are that narrowing or they are not this run's rows: a row missing
/// `disks` and `bootConfig` restored as a whole document is worse than no row at all.
fn wanted_select(kind: &'static Kind) -> Option<&'static str> {
    kind.select
}

/// The `&'static str` a stored filter names, or `None` when no page declares it any more.
fn static_filter(filter: &str) -> Option<&'static str> {
    nutsh_catalog::PAGES
        .iter()
        .flat_map(|p| p.panes.iter())
        .filter_map(|pane| pane.filter)
        .find(|f| *f == filter)
}

/// This process's 16-hex nonce, computed once. It ties the table files one process wrote to
/// the `meta.json` that vouches for them, so an orphan another process left behind is
/// recognised rather than merged.
fn session_nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "{:016x}",
            fnv1a64(&format!("{}-{nanos}", std::process::id()))
        )
    })
}

/// Delete every secret-shaped key, at every depth, in place.
///
/// The rule is `nutsh_catalog::secret`, shared with the fixture recorder and the generator;
/// the *walk* is this crate's, because the three do different things with a hit - the recorder
/// substitutes a placeholder and rewrites hosts, the generator refuses a column, the cache
/// deletes the key outright. `$`-prefixed keys carry schema identity and are always kept: a
/// `$objectType` is what makes a polymorphic row readable.
fn scrub(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // `Map::retain` hands the key as `&String` and the value as `&mut Value`, so the
            // comparisons go through `as_str`.
            map.retain(|k, v| {
                if k.starts_with('$') {
                    return true;
                }
                // Case-insensitive, the way `is_secret_name` is: the same subtree is
                // `guestCustomization` on a VM and `guestcustomization` in a template body.
                if k.eq_ignore_ascii_case(secret::OPAQUE_KEY) {
                    return false;
                }
                if secret::is_secret_name(k) {
                    return false;
                }
                // A bare `key` is usually half a category pair and occasionally a PEM blob.
                !(k.as_str() == "key"
                    && matches!(v, Value::String(s) if secret::is_key_material(s.as_str())))
            });
            for v in map.values_mut() {
                scrub(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                scrub(v);
            }
        }
        _ => {}
    }
}

/// Write the directory. Table files first, then `meta.json`, then the sweep of any `t-*.json`
/// the new meta does not name.
///
/// Within one process that ordering means a meta never vouches for a file that is not there
/// yet. **Across processes it guarantees nothing**, which is why a missing file is an ordinary
/// outcome on read: two nutsh on one context are safe but not cooperative - every write is
/// atomic, no read is ever torn, and the last to write `meta.json` owns the directory. No lock
/// file.
pub fn write(dir: &Path, snap: Snapshot, now: u64) -> anyhow::Result<()> {
    // A newer format is left entirely alone: not read, not written, not evicted.
    if let Some(existing) = read_meta(dir)
        && existing.format > FORMAT
    {
        tracing::debug!(
            dir = %dir.display(),
            format = existing.format,
            "cache written by a newer nutsh; leaving it alone"
        );
        return Ok(());
    }
    nutsh_config::atomic::create_dir_private(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let session = session_nonce().to_string();
    let mut records: Vec<TableRecord> = Vec::new();
    // `snap` is moved through: the caller built it by cloning the store's rows and drops it the
    // moment this returns, so nothing here clones a row a second time.
    for t in snap.tables {
        let mut rows: Vec<Value> = t.rows.into_iter().take(MAX_ROWS_PER_TABLE).collect();
        for row in &mut rows {
            scrub(row);
        }
        let file = table_file_name(&t.kind, &t.parents, t.filter.as_deref());
        let payload = TableFile {
            format: FORMAT,
            session: session.clone(),
            kind: t.kind,
            parents: t.parents,
            filter: t.filter,
            select: t.select,
            total: t.total,
            at: t.at,
            rows,
        };
        let text = serde_json::to_string(&payload)
            .with_context(|| format!("serializing the {} table", payload.kind))?;
        let bytes = text.len() as u64;
        // A table that serializes larger is skipped, not truncated further: its record is
        // simply absent, the final sweep unlinks any file an earlier write left for it, and the
        // next run walks it.
        if bytes > MAX_TABLE_BYTES {
            tracing::debug!(kind = %payload.kind, bytes, "table over the per-table cap; not cached");
            continue;
        }
        let path = dir.join(&file);
        nutsh_config::atomic::write_private(&path, &text)
            .with_context(|| format!("writing {}", path.display()))?;
        records.push(TableRecord {
            file,
            kind: payload.kind,
            parents: payload.parents,
            filter: payload.filter,
            select: payload.select,
            rows: payload.rows.len(),
            total: payload.total,
            bytes,
            at: payload.at,
        });
    }
    // Oldest-`at` first until the directory is under its cap.
    records.sort_by_key(|r| std::cmp::Reverse(r.at));
    let mut kept: Vec<TableRecord> = Vec::new();
    let mut total = 0u64;
    for r in records {
        if total + r.bytes > MAX_DIR_BYTES {
            let _ = std::fs::remove_file(dir.join(&r.file));
            continue;
        }
        total += r.bytes;
        kept.push(r);
    }
    kept.sort_by(|a, b| a.file.cmp(&b.file));
    let meta = Meta {
        format: FORMAT,
        written: now,
        session,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        identity: snap.identity,
        pins: snap.pins,
        namespaces: snap.namespaces.into_iter().filter(|n| n.ok).collect(),
        cluster: snap.cluster,
        names: snap.names.into_iter().take(MAX_NAMES).collect(),
        stats: snap.stats,
        sample: snap.sample,
        can_i: snap.can_i,
        tables: kept,
    };
    let text = serde_json::to_string(&meta).context("serializing meta.json")?;
    let meta_path = dir.join("meta.json");
    nutsh_config::atomic::write_private(&meta_path, &text)
        .with_context(|| format!("writing {}", meta_path.display()))?;
    // Anything the new meta does not name, including a file this write skipped.
    let named: BTreeSet<&str> = meta.tables.iter().map(|t| t.file.as_str()).collect();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("reading {}", dir.display()))?
            .path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("t-") && name.ends_with(".json") && !named.contains(name) {
            let _ = std::fs::remove_file(&path);
        }
    }
    nutsh_config::atomic::sync_dir(dir).with_context(|| format!("syncing {}", dir.display()))
}

fn read_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()
}

/// What `meta.json` says, without judging it and without removing anything: `nutsh cache info`
/// reports what is there, including a directory this build would refuse to use.
pub fn peek(dir: &Path) -> Option<Peeked> {
    let meta = read_meta(dir)?;
    Some(Peeked {
        format: meta.format,
        app_version: meta.app_version,
        written: meta.written,
        identity: meta.identity,
        pins: meta.pins.len(),
        tables: meta.tables,
    })
}

#[derive(Debug, Clone)]
pub struct Peeked {
    pub format: u32,
    /// The `nutsh` that wrote it. Reported beside `format` because [`read`] discards on both:
    /// between them they are why a directory that is plainly there is not used.
    pub app_version: String,
    pub written: u64,
    pub identity: Identity,
    pub pins: usize,
    pub tables: Vec<TableRecord>,
}

/// Everything usable in `dir`, or `None` and - except for a newer format - a directory that is
/// gone. `target` is the half of the identity known before the network is touched; the domain
/// manager and the PC version are compared by the caller once `connect` returns.
pub fn read(dir: &Path, target: &Identity, now: u64, max_age: u64) -> Option<Restored> {
    let discard = |why: &str| -> Option<Restored> {
        tracing::debug!(dir = %dir.display(), why, "cache discarded");
        discard_cache_files(dir);
        None
    };
    let Some(meta) = read_meta(dir) else {
        return discard("no readable meta.json");
    };
    if meta.format > FORMAT {
        tracing::debug!(
            dir = %dir.display(),
            format = meta.format,
            "cache written by a newer nutsh; leaving it alone"
        );
        return None;
    }
    if meta.format != FORMAT {
        return discard("older format");
    }
    if meta.app_version != env!("CARGO_PKG_VERSION") {
        return discard("another nutsh version wrote it");
    }
    if !meta.identity.same_target(target) {
        return discard("another host, port or account");
    }
    if now.saturating_sub(meta.written) > max_age {
        return discard("older than the maximum age");
    }
    let mut tables = Vec::new();
    for record in &meta.tables {
        let Some(kind) = nutsh_catalog::kind(&record.kind) else {
            drop_file(dir, &record.file);
            continue;
        };
        // A file the meta names but that is not there is expected: another process's sweep can
        // unlink it between our read of the meta and our open of it.
        let Ok(text) = std::fs::read_to_string(dir.join(&record.file)) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<TableFile>(&text) else {
            drop_file(dir, &record.file);
            continue;
        };
        // The fields, not the hash: a collision is caught here rather than merging two tables.
        if file.format != FORMAT
            || file.session != meta.session
            || file.kind != record.kind
            || file.parents != record.parents
            || file.filter != record.filter
        {
            drop_file(dir, &record.file);
            continue;
        }
        if file.select.as_deref() != wanted_select(kind) {
            drop_file(dir, &record.file);
            continue;
        }
        let filter = match &file.filter {
            Some(f) => match static_filter(f) {
                Some(s) => Some(s),
                None => {
                    drop_file(dir, &record.file);
                    continue;
                }
            },
            None => None,
        };
        let rows: Vec<Value> = file
            .rows
            .into_iter()
            .filter(|v| {
                v.get(kind.ext_id_key)
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
            })
            .collect();
        tables.push(RestoredTable {
            kind,
            parents: file.parents,
            filter,
            rows,
            total: file.total,
            at: file.at,
        });
    }
    Some(Restored {
        dir: dir.to_path_buf(),
        written: meta.written,
        identity: meta.identity,
        pins: meta.pins,
        namespaces: meta.namespaces,
        cluster: meta.cluster,
        names: meta.names,
        stats: meta.stats,
        sample: meta.sample,
        can_i: meta.can_i,
        tables,
    })
}

fn drop_file(dir: &Path, file: &str) {
    let _ = std::fs::remove_file(dir.join(file));
}

/// Remove this directory and everything in it: what `nutsh cache clear` does for one context,
/// and what the "same host, different Prism Central" verdict does after connecting.
/// Every file the cache wrote, and nothing else. A stale, foreign or unreadable inventory is
/// thrown away here; the palette's `history.json` shares the directory and is not part of it,
/// and an upgrade that discards the cache once must not take a user's typed lines with it.
///
/// `nutsh cache clear` is the other thing, and still removes the directory whole: that is a
/// person asking for every byte to go.
fn discard_cache_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == crate::history::FILE {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

pub fn remove(dir: &Path) -> anyhow::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other.with_context(|| format!("removing {}", dir.display())),
    }
}

/// Keep the eight most recently written context directories under `root` and remove the rest.
/// Run at startup, so a machine that has visited thirty Prism Centrals does not keep thirty
/// inventories of them.
pub fn sweep(root: &Path) -> anyhow::Result<()> {
    let mut dirs: Vec<(u64, PathBuf)> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {}", root.display()))),
    };
    for entry in entries {
        let path = entry
            .with_context(|| format!("reading {}", root.display()))?
            .path();
        if !path.is_dir() {
            continue;
        }
        // A newer format is not ours to evict either.
        match read_meta(&path) {
            Some(m) if m.format > FORMAT => continue,
            Some(m) => dirs.push((m.written, path)),
            None => dirs.push((0, path)),
        }
    }
    dirs.sort_by_key(|(written, _)| std::cmp::Reverse(*written));
    for (_, path) in dirs.into_iter().skip(MAX_CONTEXT_DIRS) {
        let _ = std::fs::remove_dir_all(&path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.` and `..` are outright rejections, never hashed keys. `valid_name` permits `.`, so
    /// `..` is a legal context name, and a hashed key would silently name a directory the user
    /// cannot connect to what they typed while an unslugged one would escape the tree.
    #[test]
    fn the_three_dangerous_names_are_refused_outright() {
        assert_eq!(slug(""), None);
        assert_eq!(slug("   "), None);
        assert_eq!(slug("."), None);
        assert_eq!(slug(".."), None);
        // Three dots is a directory name like any other.
        assert!(slug("...").is_some());
    }

    /// A slug-clean short name is itself, so a directory is recognisable by eye; anything else
    /// is mapped and hashed, and never leaves the tree.
    #[test]
    fn a_clean_name_is_itself_and_anything_else_is_mapped_and_hashed() {
        assert_eq!(slug("lab").as_deref(), Some("lab"));
        assert_eq!(slug("pc-7.6_lab").as_deref(), Some("pc-7.6_lab"));
        let escaping = slug("a/b").expect("a key");
        assert!(!escaping.contains('/'), "{escaping}");
        assert!(escaping.starts_with("a_b-"), "{escaping}");
        let long = slug(&"x".repeat(60)).expect("a key");
        assert!(long.len() <= 40 + 17, "{long} is {} bytes", long.len());
        assert!(long.contains('-'), "a hash was appended: {long}");
        // An ad-hoc session keys on `user@host:port`, which is never slug-clean.
        let adhoc = slug("admin@pc.lab.example:9440").expect("a key");
        assert!(!adhoc.contains('@') && !adhoc.contains(':'), "{adhoc}");
        // Deterministic: the same name is the same directory on the next run.
        assert_eq!(slug("a/b"), slug("a/b"));
        assert_ne!(slug("a/b"), slug("a_b"));
    }

    /// The file name is the hash of the three fields `TableKey`'s `Hash` and `Eq` are written
    /// over, and the fields are re-checked on read, so a collision merges nothing.
    #[test]
    fn a_table_file_is_named_by_kind_parents_and_filter() {
        let a = table_file_name("vmm.ahv.config.Vm", &[], None);
        let b = table_file_name("vmm.ahv.config.Vm", &["p".into()], None);
        let c = table_file_name("vmm.ahv.config.Vm", &[], Some("powerState eq 'ON'"));
        assert!(a.starts_with("t-") && a.ends_with(".json"), "{a}");
        assert_eq!(a.len(), "t-".len() + 16 + ".json".len());
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
        assert_eq!(a, table_file_name("vmm.ahv.config.Vm", &[], None));
        // The separator is what keeps two different splits of the same text apart.
        assert_ne!(
            table_file_name("k", &["a".into(), "b".into()], None),
            table_file_name("k", &["a\u{0}b".into()], None)
        );
    }
}
