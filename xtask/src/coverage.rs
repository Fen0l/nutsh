//! Writes the v4 coverage tables from the committed catalog.
//!
//! Reads `nutsh_catalog`, so it must run after `gen-catalog`, not before: it reports what the
//! catalog currently holds rather than what `specs/` would produce.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use nutsh_catalog::{
    CURATED_GROUPS, KINDS, NAMESPACES, NAV, NavTarget, OPERATIONS_COVERED, OPERATIONS_DECLARED,
    Reach, reach,
};

const START: &str = "<!-- coverage:start -->";
const END: &str = "<!-- coverage:end -->";

#[derive(Default)]
struct Row {
    kinds: usize,
    openable: usize,
    in_menu: usize,
    columns: usize,
    detail: usize,
    actions: usize,
}

fn tally() -> (BTreeMap<&'static str, Row>, Row) {
    // Only the curated groups: the generated namespace groups hold every openable kind this
    // file left over, so counting them would just restate `openable`.
    let menu: Vec<&str> = NAV[..CURATED_GROUPS]
        .iter()
        .flat_map(|g| g.items)
        .filter_map(|i| match i.target {
            NavTarget::Kind(id) => Some(id),
            _ => None,
        })
        .collect();

    let mut per: BTreeMap<&'static str, Row> = BTreeMap::new();
    let mut all = Row::default();
    for k in KINDS {
        let r = per.entry(k.namespace).or_default();
        for t in [&mut *r, &mut all] {
            t.kinds += 1;
            t.actions += k.actions.len();
            if reach(k) == Reach::Direct {
                t.openable += 1;
            }
            if menu.contains(&k.id) {
                t.in_menu += 1;
            }
            if !k.columns.is_empty() {
                t.columns += 1;
            }
            if !k.detail.is_empty() {
                t.detail += 1;
            }
        }
    }
    (per, all)
}

fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "-".to_string();
    }
    format!("{:.0}%", part as f64 / whole as f64 * 100.0)
}

fn summary(all: &Row) -> String {
    let from_parent = KINDS
        .iter()
        .filter(|k| matches!(reach(k), Reach::FromParent(_)))
        .count();
    let needs_param = KINDS
        .iter()
        .filter(|k| reach(k) == Reach::NeedsParameter)
        .count();
    let mut s = String::new();
    let _ = writeln!(
        s,
        "**Breadth** - how much of the v4 API the catalog models.\n"
    );
    let _ = writeln!(s, "| | | |");
    let _ = writeln!(s, "|---|---:|---:|");
    let _ = writeln!(
        s,
        "| operations the pinned specs declare | {OPERATIONS_DECLARED} | |"
    );
    let _ = writeln!(
        s,
        "| modelled by the catalog | {OPERATIONS_COVERED} | {} |",
        pct(OPERATIONS_COVERED, OPERATIONS_DECLARED)
    );
    let _ = writeln!(s, "| namespaces | {} | 100% |", NAMESPACES.len());
    let _ = writeln!(s, "| kinds | {} | |", all.kinds);
    let _ = writeln!(
        s,
        "| openable from the palette | {} | {} |",
        all.openable,
        pct(all.openable, all.kinds)
    );
    let _ = writeln!(s, "| reached by drilling into a parent | {from_parent} | |");
    let _ = writeln!(
        s,
        "| need a parameter nutsh cannot supply | {needs_param} | |"
    );
    let _ = writeln!(s, "| actions | {} | |", all.actions);
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "**Depth** - how many of those kinds have a view somebody designed, rather than one \
         derived from the schema.\n"
    );
    let _ = writeln!(s, "| | | |");
    let _ = writeln!(s, "|---|---:|---:|");
    let _ = writeln!(
        s,
        "| named in a curated menu group | {} | {} |",
        all.in_menu,
        pct(all.in_menu, all.kinds)
    );
    let _ = writeln!(
        s,
        "| with hand-picked columns | {} | {} |",
        all.columns,
        pct(all.columns, all.kinds)
    );
    let _ = writeln!(
        s,
        "| with a written detail layout | {} | {} |",
        all.detail,
        pct(all.detail, all.kinds)
    );
    let _ = writeln!(
        s,
        "| falling back to schema-derived columns | {} | {} |",
        all.kinds - all.columns,
        pct(all.kinds - all.columns, all.kinds)
    );
    s
}

fn table(per: &BTreeMap<&'static str, Row>, all: &Row) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "| namespace | version | kinds | openable | in menu | columns | detail | actions |"
    );
    let _ = writeln!(s, "|---|---|---:|---:|---:|---:|---:|---:|");
    for (ns, r) in per {
        let version = NAMESPACES
            .iter()
            .find(|n| &n.name == ns)
            .map_or("-", |n| n.version);
        let _ = writeln!(
            s,
            "| `{ns}` | {version} | {} | {} | {} | {} | {} | {} |",
            r.kinds, r.openable, r.in_menu, r.columns, r.detail, r.actions
        );
    }
    let _ = writeln!(
        s,
        "| **total** | | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** |",
        all.kinds, all.openable, all.in_menu, all.columns, all.detail, all.actions
    );
    s
}

/// Replaces the block between the markers, leaving the rest of the file alone.
fn splice(text: &str, body: &str, file: &str) -> Result<String> {
    let (Some(a), Some(b)) = (text.find(START), text.find(END)) else {
        bail!("{file} has no {START} / {END} markers");
    };
    if b < a {
        bail!("{file} has {END} before {START}");
    }
    Ok(format!(
        "{}{START}\n{body}{END}{}",
        &text[..a],
        &text[b + END.len()..]
    ))
}

pub fn generate(root: &Path) -> Result<()> {
    let (per, all) = tally();

    let readme_path = root.join("README.md");
    let readme = std::fs::read_to_string(&readme_path)
        .with_context(|| format!("reading {}", readme_path.display()))?;
    let body = format!(
        "{}\n[docs/coverage.md](docs/coverage.md) breaks this down per namespace.\n",
        summary(&all)
    );
    std::fs::write(&readme_path, splice(&readme, &body, "README.md")?)
        .with_context(|| format!("writing {}", readme_path.display()))?;

    let doc = format!(
        "# v4 coverage\n\n\
         Generated by `cargo xtask gen-coverage` from the committed catalog. `make ci` fails if \
         it is stale.\n\n\
         **openable** is a kind the palette can open on its own; the rest are reached by \
         drilling into a parent, or need a parameter nutsh has no way to supply. **in menu** is \
         a kind named in the curated sidebar rather than left to its namespace group.\n\n\
         {}\n{}",
        summary(&all),
        table(&per, &all)
    );
    let doc_path = root.join("docs/coverage.md");
    std::fs::write(&doc_path, doc).with_context(|| format!("writing {}", doc_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_replaces_only_the_marked_block() {
        let text = format!("head\n{START}\nold\n{END}\ntail\n");
        let out = splice(&text, "new\n", "t.md").unwrap();
        assert_eq!(out, format!("head\n{START}\nnew\n{END}\ntail\n"));
    }

    #[test]
    fn splice_refuses_a_file_without_markers() {
        assert!(splice("no markers here", "x", "t.md").is_err());
    }

    #[test]
    fn every_namespace_in_the_catalog_has_a_row() {
        let (per, all) = tally();
        assert_eq!(per.len(), NAMESPACES.len());
        assert_eq!(all.kinds, KINDS.len());
    }
}
