//! `cargo xtask gen-catalog`: specs/ → crates/catalog/src/generated.rs.

pub mod model;
pub mod nav;
pub mod overlay;
pub mod pages;
pub mod parse;
pub mod render;
pub mod select;
pub mod since;
pub mod spec;

use std::path::Path;

use anyhow::{Context, Result};

pub fn generate(root: &Path) -> Result<()> {
    let specs_dir = root.join("specs");
    let out_path = root.join("crates/catalog/src/generated.rs");
    let overlay_path = root.join("crates/catalog/curated.toml");

    let files = spec::select_latest(&specs_dir)?;
    let mut namespaces = Vec::new();
    let mut kinds = Vec::new();
    let mut unattached = Vec::new();
    let mut schemas: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for f in &files {
        let doc = spec::load(&f.path)?;
        // Kept for the overlay checks, which ask the same questions of the same document the
        // fallback columns were derived from. One clone of `components.schemas` per namespace.
        schemas.insert(
            f.namespace.clone(),
            doc.pointer("/components/schemas")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        );
        let mut parsed = parse::parse_namespace_full(f, &doc)
            .with_context(|| format!("parsing {}", f.path.display()))?;
        let index = since::PathIndex::load(f)?;
        for k in parsed.kinds.iter_mut() {
            k.since = index.since(&k.list_path).to_string();
        }
        eprintln!(
            "{:<15} {:<8} {:>3} kinds{}{}",
            f.namespace,
            f.version,
            parsed.kinds.len(),
            if f.preview { "  (preview)" } else { "" },
            if parsed.unattached.is_empty() {
                String::new()
            } else {
                format!("  ({} unattached actions)", parsed.unattached.len())
            }
        );
        namespaces.push(model::NamespaceModel {
            name: f.namespace.clone(),
            version: f.version.clone(),
            preview: f.preview,
            versions: f.versions.clone(),
        });
        kinds.append(&mut parsed.kinds);
        unattached.append(&mut parsed.unattached);
    }

    let overlay_text = std::fs::read_to_string(&overlay_path).unwrap_or_default();
    let overlay = overlay::parse(&overlay_text)
        .with_context(|| format!("parsing {}", overlay_path.display()))?;
    overlay::claim_extra_actions(&overlay, &mut kinds, &unattached)?;
    // After the claim, so an action `extra_actions` brought in is resolved too; before `apply`,
    // so a curated `type = "reference"` that names its kind directly is never rewritten.
    parse::resolve_field_references(&mut kinds);
    overlay::apply(&overlay, &mut kinds)?;
    // Every list walk gets a budget, so no table walks a collection a Prism Central never
    // trims - 181 825 audits is 1819 pages, and the scheduler's blanket 200-page cap still
    // spent 170 s a cycle on them. The default is written
    // into the kind here rather than read as a `None` by `Subscription::list`, so that
    // `Kind::max_rows` in the generated catalog states the walk the scheduler will actually
    // do; see `catalog::DEFAULT_MAX_ROWS`. Curated budgets are already in place and win.
    for k in kinds.iter_mut() {
        k.max_rows.get_or_insert(nutsh_catalog::DEFAULT_MAX_ROWS);
    }
    overlay::resolve_schemas(&overlay, &mut kinds, &schemas)?;
    overlay::check(&kinds, &schemas)?;
    kinds.sort_by(|a, b| a.id.cmp(&b.id));

    let pages_path = root.join("crates/catalog/pages.toml");
    let pages_text = std::fs::read_to_string(&pages_path)
        .with_context(|| format!("reading {}", pages_path.display()))?;
    let pages =
        pages::parse(&pages_text).with_context(|| format!("parsing {}", pages_path.display()))?;
    pages::validate(&pages, &kinds)?;
    // After the pages, because a pane may draw a column the kind's own table does not, and
    // before the render, which is the only thing that reads the field.
    select::apply(&mut kinds, &pages);
    let nav_path = root.join("crates/catalog/nav.toml");
    let nav_text = std::fs::read_to_string(&nav_path)
        .with_context(|| format!("reading {}", nav_path.display()))?;
    let nav = nav::parse(&nav_text).with_context(|| format!("parsing {}", nav_path.display()))?;
    nav::validate(&nav, &kinds, &pages)?;
    // Counted before the per-namespace groups are appended: it is what the sidebar's digit
    // keys cover, and `1`..`9` must not run into a generated group.
    let curated = nav.groups.len();
    let groups = nav::with_namespace_groups(nav, &kinds);

    let src = render::render(&namespaces, &kinds, &groups, curated, &pages)?;
    std::fs::write(&out_path, src).with_context(|| format!("writing {}", out_path.display()))?;
    eprintln!(
        "wrote {} ({} namespaces, {} kinds)",
        out_path.display(),
        namespaces.len(),
        kinds.len()
    );
    // Never fatal: an unattached action is a missing feature, not drift.
    let claimed: std::collections::HashSet<&str> = overlay
        .kinds
        .values()
        .flat_map(|o| o.extra_actions.iter().map(String::as_str))
        .collect();
    let left: Vec<&str> = unattached
        .iter()
        .map(|a| a.path.as_str())
        .filter(|p| !claimed.contains(p))
        .collect();
    if !left.is_empty() {
        eprintln!("{} unattached $actions paths:", left.len());
        for p in &left {
            eprintln!("  {p}");
        }
    }
    Ok(())
}
