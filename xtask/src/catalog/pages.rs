//! `crates/catalog/pages.toml`: the feature pages, validated against the kinds.

use anyhow::{Result, bail};
use serde::Deserialize;

use super::model::{ColumnModel, KindModel};

/// At most this many subscriptions per page: six lists every few seconds is already the most a
/// screen should ask a Prism Central for.
const MAX_PANES: usize = 6;

/// At most this many panes side by side in one grid row: a third column of a 120-cell page is
/// forty cells, which is under the width a table is laid out for.
const MAX_COLS: u16 = 2;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PagesFile {
    #[serde(default, rename = "page")]
    pub pages: Vec<PageModel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageModel {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub summary_rows: u16,
    #[serde(default)]
    pub row_heights: Vec<u16>,
    #[serde(default, rename = "pane")]
    pub panes: Vec<PaneModel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneModel {
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub columns: Vec<ColumnModel>,
    pub filter: Option<String>,
    pub orderby: Option<String>,
    pub row: u16,
    pub col: u16,
    pub weight: u16,
    pub empty: String,
    /// Why this pane's columns are not a subsequence of the kind's curated ones. A comment the
    /// generator can read: `deny_unknown_fields` means a pane cannot differ by accident, and
    /// this field is not rendered into `PaneDef` - it exists to be justified, not to be drawn.
    /// A blank note is refused, and so is one on a pane that no longer differs.
    #[serde(default)]
    pub columns_note: Option<String>,
}

pub fn parse(text: &str) -> Result<PagesFile> {
    Ok(toml::from_str(text)?)
}

pub fn validate(pages: &PagesFile, kinds: &[KindModel]) -> Result<()> {
    let mut problems = Vec::new();
    for p in &pages.pages {
        if p.panes.len() > MAX_PANES {
            problems.push(format!(
                "{}: {} panes, at most {MAX_PANES}",
                p.id,
                p.panes.len()
            ));
        }
        if p.row_heights.is_empty() {
            problems.push(format!("{}: row_heights is empty", p.id));
        }
        if usize::from(p.summary_rows) > p.row_heights.len() {
            problems.push(format!("{}: summary_rows past the last row", p.id));
        }
        let mut cells: Vec<(u16, u16)> = Vec::new();
        for pane in &p.panes {
            // One lookup, asked once: the same question answers "is this kind known" and
            // "what does it curate".
            let of_kind = kinds.iter().find(|k| k.id == pane.kind);
            if of_kind.is_none() {
                problems.push(format!("{}: unknown kind id {}", p.id, pane.kind));
            }
            if usize::from(pane.row) >= p.row_heights.len() {
                problems.push(format!("{}: {} is on row {}", p.id, pane.title, pane.row));
            }
            if pane.col >= MAX_COLS {
                problems.push(format!("{}: {} is in col {}", p.id, pane.title, pane.col));
            }
            // Two panes in one cell would draw over each other: the layout engine gives a
            // cell to one pane.
            if cells.contains(&(pane.row, pane.col)) {
                problems.push(format!(
                    "{}: {} repeats cell ({}, {})",
                    p.id, pane.title, pane.row, pane.col
                ));
            }
            cells.push((pane.row, pane.col));
            // A weight of zero is a pane with no width, which is a pane nobody asked for.
            if pane.weight == 0 {
                problems.push(format!("{}: {} has weight 0", p.id, pane.title));
            }
            for c in &pane.columns {
                if !is_path(&c.path) {
                    problems.push(format!("{}: {:?} is not a dotted path", p.id, c.path));
                }
            }
            // A note that says nothing justifies nothing, and `deny_unknown_fields` cannot
            // tell an empty string from a reason.
            if pane
                .columns_note
                .as_deref()
                .is_some_and(|n| n.trim().is_empty())
            {
                problems.push(format!(
                    "{}: {}'s columns_note is blank; it exists to be justified",
                    p.id, pane.title
                ));
            }
            let note = pane
                .columns_note
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty());
            // The two curation sites are not redundant - a pane is usually narrower than a
            // table, takes fewer columns and renames their headers (`MESSAGE` for the Alert's
            // `TITLE`, `AGE` for its `CREATED`), which is why this compares paths and kinds and
            // ignores headers - but they must not drift silently. A kind with no curated
            // columns constrains nothing.
            if let Some(k) = of_kind
                && !k.columns.is_empty()
            {
                match (note, is_subsequence(&pane.columns, &k.columns)) {
                    (None, false) => problems.push(format!(
                        "{}: {}'s columns are not a subsequence of {}'s curated ones; \
                         reorder them, curate the kind, or say why with columns_note",
                        p.id, pane.title, pane.kind
                    )),
                    // The exemption is not permanent: a pane that has come back into line - or
                    // a kind curated into agreement with it - drops the note, so the check
                    // comes back on rather than staying off for good.
                    (Some(_), true) => problems.push(format!(
                        "{}: {}'s columns_note is no longer needed; they are a subsequence of \
                         {}'s curated ones",
                        p.id, pane.title, pane.kind
                    )),
                    _ => {}
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("pages.toml: {}", problems.join("; "))
    }
}

/// Whether `pane` appears inside `curated` in order, comparing the path and the kind and
/// ignoring the header - a pane renames a column freely (`MESSAGE` for the Alert's `TITLE`,
/// `AGE` for its `CREATED`) and that is the point of having pane columns at all.
fn is_subsequence(pane: &[ColumnModel], curated: &[ColumnModel]) -> bool {
    let mut it = curated.iter();
    pane.iter()
        .all(|p| it.any(|c| c.path == p.path && c.kind == p.kind))
}

/// The grammar `nutsh_core::path::get` walks: dot-separated segments, each optionally ending
/// `[]`, none of them empty.
fn is_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('.').all(|segment| {
            let key = segment.strip_suffix("[]").unwrap_or(segment);
            !key.is_empty() && !key.contains(['[', ']'])
        })
}
