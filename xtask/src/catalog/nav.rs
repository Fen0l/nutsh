//! `crates/catalog/nav.toml`: the sidebar's tree, validated against the kinds and the pages.

use anyhow::{Result, bail};
use serde::Deserialize;

use super::model::KindModel;
use super::pages::PagesFile;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavFile {
    #[serde(default, rename = "group")]
    pub groups: Vec<GroupModel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupModel {
    pub name: String,
    #[serde(default)]
    pub items: Vec<ItemModel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemModel {
    pub label: String,
    pub kind: Option<String>,
    pub page: Option<String>,
    pub note: Option<String>,
    /// The Contexts screen.
    #[serde(default)]
    pub contexts: bool,
    /// The settings screen.
    #[serde(default)]
    pub settings: bool,
}

/// The one page id that is allowed to name a group too; see [`validate`].
const DASHBOARD_PAGE: &str = "dashboard";

pub fn parse(text: &str) -> Result<NavFile> {
    Ok(toml::from_str(text)?)
}

/// `catalog::reach(k) == Reach::Direct`, over a `KindModel`.
///
/// The catalog's own `reach` reads the committed `generated.rs`, which is the *previous*
/// catalog during a regeneration, so validating `nav.toml` with it would check the tree
/// against stale data. `the_generators_direct_predicate_agrees_with_the_catalogs` keeps the
/// two in step.
pub fn is_direct(k: &KindModel) -> bool {
    !k.list_params.required
        && k.parent.is_none()
        && nutsh_catalog::placeholder_count(&k.list_path) == 0
}

/// Three rules, all reported at once: every `kind` resolves, every `page` exists, and every
/// `kind` is openable from the top level. The third is not theoretical - it caught nine items
/// in the first draft of this tree that read like Prism Central pages but come back
/// `FromParent` or `NeedsParameter`.
pub fn validate(nav: &NavFile, kinds: &[KindModel], pages: &PagesFile) -> Result<()> {
    let mut problems = Vec::new();
    // A fourth rule, about the two files together. `App::nav_id` resolves a page id **exactly**
    // and a group name **case-insensitively**, in that order, so a page id that also names a
    // group takes `:hide <that name>` away from the group for good - the group becomes
    // unhideable and the page is hidden in its place, silently. The one pair that exists is
    // `dashboard` / `Dashboard`, and it is safe because neither half of it can be hidden at all
    // (`sidebar::protected`); any other pair is the same bug without the guard, which is why a
    // landing page is `hardware-overview` rather than `hardware`. A page's *title* is free to be
    // the group's name - `nav_id` never looks at a title, and `resolve_home` does, which is what
    // makes `nutsh Hardware` open the page.
    for p in &pages.pages {
        if p.id == DASHBOARD_PAGE {
            continue;
        }
        if let Some(g) = nav
            .groups
            .iter()
            .find(|g| g.name.eq_ignore_ascii_case(&p.id))
        {
            problems.push(format!(
                "page {} also names the group {:?}; `:hide {}` could then never reach the group",
                p.id, g.name, p.id
            ));
        }
    }
    for g in &nav.groups {
        for item in &g.items {
            // Exactly one of the four, and the arms say so: a `note` beside a `kind` would be
            // generated onto an item the sidebar never draws a reason for.
            match (
                &item.kind,
                &item.page,
                item.contexts || item.settings,
                &item.note,
            ) {
                (Some(id), None, false, None) => match kinds.iter().find(|k| &k.id == id) {
                    None => problems.push(format!("{}: unknown kind id {id}", item.label)),
                    Some(k) if !is_direct(k) => problems.push(format!(
                        "{}: {id} is not Reach::Direct; drill into its parent instead",
                        item.label
                    )),
                    Some(_) => {}
                },
                (None, Some(id), false, None) => {
                    if !pages.pages.iter().any(|p| &p.id == id) {
                        problems.push(format!("{}: unknown page {id}", item.label));
                    }
                }
                (None, None, true, None) => {}
                (None, None, false, Some(_)) => {}
                _ => problems.push(format!(
                    "{}: exactly one of kind, page, contexts, settings or note",
                    item.label
                )),
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("nav.toml: {}", problems.join("; "))
    }
}

/// The curated groups, then one per namespace holding what the curated tree left over of it,
/// sorted by display name. The bare namespace is the group name on purpose: a composite like
/// `Infrastructure (clustermgmt)` is thirty cells and the sidebar has twenty-two.
///
/// **What `nav.toml` placed by hand is not placed again.** Appending every directly openable
/// kind of a namespace made the lower half a second copy of the upper one: all 71 curated kind
/// rows appeared twice, under a curated label above and under a raw namespace below, with
/// `datapolicies` and `dataprotection` sitting beneath the `Data Protection` they duplicated.
/// The point of these groups is that nothing in the catalog is unreachable from the menu, and
/// a kind the tree already reaches is not unreachable - so the namespaces hold the remainder,
/// which is an explorer for the 119 kinds nobody has curated, and a namespace with no remainder
/// contributes no group at all.
pub fn with_namespace_groups(nav: NavFile, kinds: &[KindModel]) -> Vec<GroupModel> {
    let placed: std::collections::HashSet<String> = nav
        .groups
        .iter()
        .flat_map(|g| g.items.iter())
        .filter_map(|i| i.kind.clone())
        .collect();
    let mut groups = nav.groups;
    let mut namespaces: Vec<&str> = kinds.iter().map(|k| k.namespace.as_str()).collect();
    namespaces.sort_unstable();
    namespaces.dedup();
    for ns in namespaces {
        let mut items: Vec<&KindModel> = kinds
            .iter()
            .filter(|k| k.namespace == ns && is_direct(k) && !placed.contains(&k.id))
            .collect();
        if items.is_empty() {
            continue;
        }
        items.sort_by(|a, b| a.display.cmp(&b.display).then(a.id.cmp(&b.id)));
        groups.push(GroupModel {
            name: ns.to_string(),
            items: items
                .into_iter()
                .map(|k| ItemModel {
                    label: k.display.clone(),
                    kind: Some(k.id.clone()),
                    page: None,
                    note: None,
                    contexts: false,
                    settings: false,
                })
                .collect(),
        });
    }
    groups
}
