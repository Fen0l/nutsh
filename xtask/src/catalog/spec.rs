//! Pick one spec file per namespace and load it as JSON.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nutsh_catalog::version_key;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct SpecFile {
    pub namespace: String,
    pub version: String,
    pub preview: bool,
    pub path: PathBuf,
    /// GA versions of this namespace newest first, or just `version` for a preview-only
    /// namespace.
    pub versions: Vec<String>,
}

/// `v4.3` is GA; `v4.3.b1` and `v4.0.a3` are pre-release.
pub fn is_ga(version: &str) -> bool {
    let Some(rest) = version.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// For each namespace directory pick the newest GA version, else the newest
/// pre-release flagged `preview`. Directories without `.yaml` files are skipped.
pub fn select_latest(specs_dir: &Path) -> Result<Vec<SpecFile>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(specs_dir)
        .with_context(|| format!("reading {}", specs_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut out = Vec::new();
    for dir in dirs {
        let namespace = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut versions: Vec<String> = std::fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter_map(|f| f.strip_suffix(".yaml").map(str::to_string))
            .collect();
        if versions.is_empty() {
            continue;
        }
        // The string is a tie-breaker so two versions with equal keys still sort the same
        // way on every machine: `generated.rs` must not depend on `read_dir` order.
        versions.sort_by_key(|v| (version_key(v), v.clone()));
        let ga: Vec<String> = versions.iter().filter(|v| is_ga(v)).cloned().collect();
        let (version, preview, served) = match ga.last() {
            Some(v) => (v.clone(), false, ga.iter().rev().cloned().collect()),
            None => {
                let v = versions.last().cloned().unwrap_or_default();
                (v.clone(), true, vec![v])
            }
        };
        out.push(SpecFile {
            path: dir.join(format!("{version}.yaml")),
            namespace,
            version,
            preview,
            versions: served,
        });
    }
    Ok(out)
}

pub fn load(path: &Path) -> Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_yaml_ng::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}
