//! `Kind::select`: the top-level properties a table row is read for, and nothing else.
//!
//! A list of a hundred virtual machines carries every disk, every NIC, every boot entry and
//! every category of every one of them, and a table draws six columns. `$select` is how the
//! Prism Central is asked for the six.
//!
//! Two things make that safe rather than merely smaller, and both are decided here.
//!
//! The first is that the union below is what the **code** reads, not what a curator happened to
//! list. A column is the obvious member; the identifier, the name, the addresses
//! `core::search` matches a term against, the field a change-probe orders by, the paths an
//! action fills its parent placeholders from and the flag that says whether a task may be
//! cancelled are all read off a row too, by code a curator never sees. Leaving any of them out
//! blanks something quietly, so they are all in, and `crates/catalog/tests/consistency.rs`
//! asserts each of them against the generated catalog.
//!
//! The second is that a composed detail is drawn over the row the store holds, so narrowing a
//! list narrows the detail with it unless something else can hand the store a whole document.
//! A single-entity GET is that something, and a kind with no `get_path` has none: those are not
//! narrowed at all. Neither is a kind whose endpoint declares no `$select`, one whose schema
//! could not be found, one that reads a property its schema does not declare, or one whose
//! union names every property there is.

use std::collections::BTreeSet;

use crate::catalog::model::{ColumnModel, KindModel};
use crate::catalog::pages::PagesFile;

/// Fill in `KindModel::select` for every kind. Called once the pages are parsed, because a page
/// pane may draw a column the kind's own table does not.
pub fn apply(kinds: &mut [KindModel], pages: &PagesFile) {
    let panes: Vec<(&str, &[ColumnModel])> = pages
        .pages
        .iter()
        .flat_map(|p| p.panes.iter())
        .map(|pane| (pane.kind.as_str(), pane.columns.as_slice()))
        .collect();
    for k in kinds.iter_mut() {
        let extra: Vec<&ColumnModel> = panes
            .iter()
            .filter(|(kind, _)| *kind == k.id)
            .flat_map(|(_, columns)| columns.iter())
            .collect();
        k.select = select_for(k, &extra);
    }
}

/// The head of a dotted path: `disks[].backingInfo.diskSizeBytes` is asked for as `disks`.
///
/// `$select` in these specs is a list of properties of the entity, and every nested read the
/// table does is under one of them, so the head is both the smallest thing that can be asked
/// for and the whole of what is needed.
fn root(path: &str) -> &str {
    let head = path.split(['.', '[']).next().unwrap_or(path);
    head.trim()
}

fn select_for(k: &KindModel, pane_columns: &[&ColumnModel]) -> Option<String> {
    // A composed detail can only be made whole again by a single-entity GET. Without one,
    // everything the columns do not name is lost for good.
    k.get_path.as_ref()?;
    if !k.list_params.select || k.properties.is_empty() {
        return None;
    }
    let declared: BTreeSet<&str> = k.properties.iter().map(String::as_str).collect();
    let mut want: BTreeSet<&str> = BTreeSet::new();

    // The identifier. `Entity::new` reads it out of the row, and a row without one has no
    // identity at all: no cursor, no cache entry, no action.
    want.insert(k.ext_id_key.as_str());
    // The name, both ways it is found: the curated path first, then the `NAME_KEYS` walk. A
    // name that falls through both becomes the extId, which is what a table full of UUIDs
    // looks like, so every key the walk could reach is asked for where the schema has it.
    if !k.name_path.is_empty() {
        want.insert(root(&k.name_path));
    }
    want.extend(
        nutsh_catalog::NAME_KEYS
            .iter()
            .copied()
            .filter(|key| declared.contains(key)),
    );
    // The columns the table draws. Curated ones replace the derived ones rather than extending
    // them (`tui::table::columns`), so only the set on screen is asked for; `w` widens within
    // that same set.
    let shown: &[ColumnModel] = if k.columns.is_empty() {
        &k.fallback_columns
    } else {
        &k.columns
    };
    want.extend(shown.iter().map(|c| root(&c.path)));
    // A page pane may draw a column the kind's own table does not.
    want.extend(pane_columns.iter().map(|c| root(&c.path)));
    // Addresses `core::search` matches a term against: the IP columns of both sets, whichever
    // is drawn, and the IP fields of the composed detail. A Host keeps its IPMI address in the
    // detail and nowhere else, and somebody holding a DRAC address searches for it.
    want.extend(
        k.columns
            .iter()
            .chain(&k.fallback_columns)
            .filter(|c| c.kind == "Ip")
            .map(|c| root(&c.path)),
    );
    want.extend(
        k.detail
            .iter()
            .flat_map(|section| section.fields.iter())
            .filter(|f| f.kind == "Ip")
            .map(|f| root(&f.path)),
    );
    // The change-probe's field: the walk reads it off every row it fetches to calibrate the
    // probe against what it saw.
    if let Some(field) = &k.probe_by {
        want.insert(root(field));
    }
    // What an action fills its leading path placeholders from: a flat Host carries the cluster
    // it is in, and `core::actions::plan` reads it off the row.
    want.extend(k.action_parents.iter().map(|p| root(p)));
    // Whether a task may be cancelled is a property of the row, read by `core::actions`.
    if k.actions.iter().any(|a| a.name == "cancel") && declared.contains("isCancelable") {
        want.insert("isCancelable");
    }

    // A root the schema does not declare cannot be asked for: the far end answers 400 on the
    // whole list, and losing the list is worse than sending a few kilobytes nobody reads. This
    // is the generator saying "I could not prove this one safe" and not narrowing it.
    if want.iter().any(|root| !declared.contains(root)) {
        return None;
    }
    // Nothing to save.
    if want.len() >= declared.len() {
        return None;
    }
    Some(want.into_iter().collect::<Vec<_>>().join(","))
}
