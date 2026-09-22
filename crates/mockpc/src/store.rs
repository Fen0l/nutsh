//! Fixtures keyed by API path, e.g. `/vmm/v4.3/ahv/config/vms` → entities.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

use serde_json::Value;

#[derive(Debug, Default, PartialEq)]
pub struct Store {
    lists: HashMap<String, Vec<Value>>,
}

impl Store {
    /// Every `<dir>/<a>/<b>/<c>.json` becomes the list at `/<a>/<b>/<c>`.
    pub fn load(dir: &Path) -> std::io::Result<Store> {
        let mut store = Store::default();
        if dir.is_dir() {
            walk(dir, dir, &mut store)?;
        }
        Ok(store)
    }

    /// The same store `load` builds from a directory, from `(path, contents)` pairs: each
    /// `<a>/<b>/<c>.json` becomes the list at `/<a>/<b>/<c>`. What an embedded fixture tree
    /// is loaded from, and what `load(dir) == from_files(table)` compares.
    pub fn from_files(files: &[(&str, &str)]) -> serde_json::Result<Store> {
        let mut store = Store::default();
        for (path, text) in files {
            let key = format!("/{}", path.strip_suffix(".json").unwrap_or(path));
            store.lists.insert(key, rows(serde_json::from_str(text)?));
        }
        Ok(store)
    }

    /// Whether a list exists at `path`, for the callers that only need to know that.
    pub(crate) fn has(&self, path: &str) -> bool {
        self.lists.contains_key(path)
    }

    pub fn list(&self, path: &str) -> Option<&[Value]> {
        self.lists.get(path).map(Vec::as_slice)
    }

    /// Owned, because the caller usually edits it and writes it back: an entity is a small
    /// object, so the clone beats an overlay map's complexity now that mutations change the
    /// store. `id_key` is the kind's `ext_id_key`: storage containers are addressed by
    /// `containerExtId` and carry no `extId` at all.
    pub fn entity(&self, list_path: &str, id_key: &str, ext_id: &str) -> Option<Value> {
        self.lists
            .get(list_path)?
            .iter()
            .find(|e| e.get(id_key).and_then(Value::as_str) == Some(ext_id))
            .cloned()
    }

    pub(crate) fn replace(&mut self, list_path: &str, id_key: &str, ext_id: &str, entity: Value) {
        if let Some(items) = self.lists.get_mut(list_path)
            && let Some(slot) = items
                .iter_mut()
                .find(|e| e.get(id_key).and_then(Value::as_str) == Some(ext_id))
        {
            *slot = entity;
        }
    }

    pub(crate) fn remove(&mut self, list_path: &str, id_key: &str, ext_id: &str) {
        if let Some(items) = self.lists.get_mut(list_path) {
            items.retain(|e| e.get(id_key).and_then(Value::as_str) != Some(ext_id));
        }
    }

    pub(crate) fn push(&mut self, list_path: &str, entity: Value) {
        self.lists
            .entry(list_path.to_string())
            .or_default()
            .push(entity);
    }

    pub fn insert(&mut self, list_path: &str, entities: Vec<Value>) {
        self.lists.insert(list_path.to_string(), entities);
    }

    /// Every list path this store holds, sorted, so a walk over the whole store reports in one
    /// order whatever `HashMap` did. The demo's integrity tests are the caller: they need to see
    /// the lists `demo::store` derived at load, which no table names.
    pub fn paths(&self) -> Vec<&str> {
        let mut paths: Vec<&str> = self.lists.keys().map(String::as_str).collect();
        paths.sort_unstable();
        paths
    }

    /// The version segment the fixture files of `namespace` carry (`v4.3` for
    /// `/vmm/v4.3/...`); `None` when no fixture is under that namespace. A tree that holds two
    /// versions of one namespace maps onto the newest, so the answer never depends on
    /// `HashMap` order.
    pub fn version_of(&self, namespace: &str) -> Option<String> {
        let prefix = format!("/{namespace}/");
        self.lists
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix)?.split('/').next())
            .max_by_key(|v| nutsh_catalog::version_key(v))
            .map(str::to_string)
    }
}

fn walk(root: &Path, dir: &Path, store: &mut Store) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(root, &path, store)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("under root")
            .with_extension("");
        let key = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
        let text = std::fs::read_to_string(&path)?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::other(format!("{}: {e}", path.display())))?;
        store.lists.insert(key, rows(value));
    }
    Ok(())
}

/// A file's rows: a JSON array is the list, and a single object is a one-row list, which is
/// how a fixture recorded from a single-entity endpoint (a domain manager) is written.
fn rows(value: Value) -> Vec<Value> {
    match value {
        Value::Array(a) => a,
        other => vec![other],
    }
}

/// Deterministic ETag for an entity body.
pub fn etag_of(entity: &Value) -> String {
    let mut h = DefaultHasher::new();
    entity.to_string().hash(&mut h);
    format!("\"{:016x}\"", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_bundled_fixtures() {
        let store = Store::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")).unwrap();
        assert_eq!(store.list("/vmm/v4.3/ahv/config/vms").unwrap().len(), 3);
        assert!(
            store
                .entity(
                    "/vmm/v4.3/ahv/config/vms",
                    "extId",
                    "3d0c4a2e-1b8f-4c1a-9e2f-000000000002"
                )
                .is_some()
        );
        assert!(store.list("/nope").is_none());
    }

    #[test]
    fn version_of_reads_the_fixture_tree() {
        let store = Store::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")).unwrap();
        assert_eq!(store.version_of("vmm").as_deref(), Some("v4.3"));
        assert_eq!(store.version_of("files"), None);
    }

    #[test]
    fn version_of_picks_the_newest_when_a_namespace_has_two() {
        let mut store = Store::default();
        store.insert("/x/v4.1/config/things", vec![]);
        store.insert("/x/v4.3/config/things", vec![]);
        assert_eq!(store.version_of("x").as_deref(), Some("v4.3"));
    }

    #[test]
    fn cluster_scoped_hosts_match_the_top_level_hosts() {
        let store = Store::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")).unwrap();
        assert_eq!(
            store.list("/clustermgmt/v4.3/config/hosts"),
            store.list(
                "/clustermgmt/v4.3/config/clusters/0006158a-2f0d-4d5a-8e2d-000000000010/hosts"
            )
        );
    }

    #[test]
    fn from_files_keys_paths_without_the_extension_and_wraps_an_object() {
        let store = Store::from_files(&[
            (
                "vmm/v4.3/ahv/config/vms.json",
                r#"[{"extId": "a"}, {"extId": "b"}]"#,
            ),
            (
                "prism/v4.4/config/domain-managers.json",
                r#"{"extId": "dm"}"#,
            ),
        ])
        .unwrap();
        assert_eq!(store.list("/vmm/v4.3/ahv/config/vms").unwrap().len(), 2);
        assert_eq!(
            store
                .list("/prism/v4.4/config/domain-managers")
                .unwrap()
                .len(),
            1,
            "a single object is a one-row list, as it is on disk"
        );
        assert!(Store::from_files(&[("x/v1/bad.json", "{")]).is_err());
    }

    /// The drift mechanism an embedded table relies on: a tree read from disk and the same
    /// files handed over as text are one store.
    #[test]
    fn load_equals_from_files_for_the_bundled_tree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut files: Vec<(String, String)> = Vec::new();
        collect(&root, &root, &mut files);
        assert!(files.len() > 20, "the walk found the tree: {}", files.len());
        let table: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        assert_eq!(
            Store::load(&root).unwrap(),
            Store::from_files(&table).unwrap()
        );
        let mut one_short = table.clone();
        one_short.pop();
        assert_ne!(
            Store::load(&root).unwrap(),
            Store::from_files(&one_short).unwrap(),
            "a file on disk that the table lacks is a difference"
        );
    }

    #[test]
    fn paths_are_sorted_and_name_every_list() {
        let mut store = Store::default();
        store.insert("/x/v4.1/config/things", vec![]);
        store.insert("/a/v4.3/config/rows", vec![]);
        assert_eq!(
            store.paths(),
            vec!["/a/v4.3/config/rows", "/x/v4.1/config/things"]
        );
    }

    fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(root, &path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&path).unwrap()));
        }
    }
}
