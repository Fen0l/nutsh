//! The committed fixtures must pass the same leak scan that gates a recording: a fixture is
//! the one artefact of a lab session that ends up in the repository.

use std::path::{Path, PathBuf};

use serde_json::Value;
use xtask::record::leaks;

/// Every committed fixture tree: `crates/mockpc/fixtures` and its siblings (`fixtures-ambiguous`,
/// and any later variant), so a new tree is scanned without touching this test.
fn fixture_trees() -> Vec<PathBuf> {
    let mockpc = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the workspace")
        .join("crates/mockpc");
    let mut trees: Vec<PathBuf> = std::fs::read_dir(&mockpc)
        .unwrap_or_else(|e| panic!("{}: {e}", mockpc.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("fixtures"))
        })
        .collect();
    trees.sort();
    trees
}

fn json_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            json_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
}

#[test]
fn committed_fixtures_pass_the_leak_scan() {
    let trees = fixture_trees();
    assert!(!trees.is_empty(), "no fixture tree under crates/mockpc");
    let mut files = Vec::new();
    for dir in &trees {
        json_files(dir, &mut files);
    }
    files.sort();
    assert!(
        !files.is_empty(),
        "no fixtures under {}",
        trees
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut findings = Vec::new();
    for file in &files {
        let text =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let value: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        // No host to match: a committed fixture belongs to no particular Prism Central.
        findings.extend(
            leaks(&value, "")
                .into_iter()
                .map(|f| format!("{}: {f}", file.display())),
        );
    }
    assert!(
        findings.is_empty(),
        "leak scan flagged {} fixture value(s):\n{}",
        findings.len(),
        findings.join("\n")
    );
}
