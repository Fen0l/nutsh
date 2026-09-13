//! `since`: the oldest GA version of a namespace whose spec has a kind's list path. Only the
//! `paths` keys of older specs are read; their operations and schemas play no part.

use std::collections::HashSet;

use anyhow::{Context, Result, anyhow};
use nutsh_catalog::with_version;

use super::parse::normalize;
use super::spec::{self, SpecFile};

/// The version stand-in every key carries, so paths from different versions compare equal.
const ANY_VERSION: &str = "{v}";

/// A path reduced to what identifies the endpoint across versions: the version segment
/// replaced by `version`, and every parameter name dropped, so that a spec which renames
/// `/three/v4.0/config/bolts/{boltId}` to `/three/v4.2/config/bolts/{extId}` still describes
/// the same endpoint and does not push the kind's `since` forward.
fn key(path: &str, version: &str) -> String {
    normalize(&with_version(path, version))
}

/// The path keys of every version older than the chosen one, oldest first, normalised by
/// `key` so that `/vmm/v4.1/ahv/config/vms` and `/vmm/v4.3/ahv/config/vms` compare equal.
pub struct PathIndex {
    oldest_first: Vec<(String, HashSet<String>)>,
    fallback: String,
}

impl PathIndex {
    /// Loads `spec.versions[1..]` from `spec.path`'s directory. The chosen version itself is
    /// not loaded: a kind that came out of it is known to be there.
    pub fn load(spec: &SpecFile) -> Result<PathIndex> {
        let dir = spec
            .path
            .parent()
            .ok_or_else(|| anyhow!("{} has no parent directory", spec.path.display()))?;
        let mut oldest_first = Vec::new();
        for version in spec.versions.iter().skip(1).rev() {
            let path = dir.join(format!("{version}.yaml"));
            let doc = spec::load(&path)?;
            let keys: HashSet<String> = doc
                .get("paths")
                .and_then(|p| p.as_object())
                .with_context(|| format!("{} has no paths", path.display()))?
                .keys()
                .map(|k| key(k, ANY_VERSION))
                .collect();
            oldest_first.push((version.clone(), keys));
        }
        Ok(PathIndex {
            oldest_first,
            fallback: spec.version.clone(),
        })
    }

    /// The oldest version whose spec has `list_path`, else the chosen version.
    pub fn since(&self, list_path: &str) -> &str {
        let wanted = key(list_path, ANY_VERSION);
        self.oldest_first
            .iter()
            .find(|(_, keys)| keys.contains(&wanted))
            .map(|(v, _)| v.as_str())
            .unwrap_or(&self.fallback)
    }
}
