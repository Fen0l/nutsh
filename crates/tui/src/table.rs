//! Rows and columns for one table view: which columns, how wide, in what order, and which of
//! them `/` has narrowed the view to.

use std::time::SystemTime;

use nutsh_catalog::{Column, ColumnKind, Kind};
use nutsh_core::cell::{self, Names};
use nutsh_core::search::Query;
use nutsh_core::store::Table;
use unicode_width::UnicodeWidthStr;

use crate::key::Key;
use crate::text::{Edit, Input};

pub use nutsh_catalog::DEFAULT_COLUMNS;
pub const WIDE_COLUMNS: usize = 20;

/// The `/` filter over one table view: the term, and whether the keys are still going into it.
///
/// It filters the rows the view already has and asks for nothing: a term is not an OData
/// `$filter` and no request is made for one. That is the promise the title's `n of m` makes -
/// `m` is what is loaded, never what the server holds - and it is why the filter lives on the
/// view rather than on the [`nutsh_core::store::TableKey`] the scheduler polls.
///
/// The parsed [`Query`] is rebuilt by the one mutator, so the term on screen and the rows under
/// it cannot drift: there is no way to change the text without re-parsing it.
#[derive(Debug)]
pub struct Filter {
    input: Input,
    /// Whether the keys still belong to the term. `enter` hands them back to the table and
    /// leaves the filter standing; `esc` takes the whole filter away.
    typing: bool,
    query: Query,
}

impl Filter {
    /// What `/` opens: an empty term with the keys in it.
    pub(crate) fn new() -> Filter {
        Filter {
            input: Input::default(),
            typing: true,
            query: Query::default(),
        }
    }

    /// One key into the term, through the one key map every text surface in this crate shares.
    /// `None` is a key the term does not own, which the view then answers for itself.
    pub(crate) fn key(&mut self, key: Key) -> Option<Edit> {
        let edit = self.input.key(key)?;
        if edit == Edit::Changed {
            self.query = Query::new(self.input.as_str());
        }
        Some(edit)
    }

    /// The keys go back to the table; the term stays.
    pub(crate) fn commit(&mut self) {
        self.typing = false;
    }

    /// `/` over a filter that is already standing: the term comes back for editing, with the
    /// cursor at its end, rather than being cleared. Clearing is what `esc` is for, and a `/`
    /// that threw away a term the user had just typed would make the two keys one.
    pub(crate) fn reopen(&mut self) {
        self.input.end();
        self.typing = true;
    }

    pub(crate) fn typing(&self) -> bool {
        self.typing
    }

    pub(crate) fn input(&self) -> &Input {
        &self.input
    }

    /// The term as typed: what the title shows beside the count.
    pub(crate) fn term(&self) -> &str {
        self.input.as_str()
    }

    /// What the rows are matched against, or `None` while nothing has been typed - a bare `/`
    /// narrows nothing, so the frame between the keystroke and the first letter is the table as
    /// it was.
    pub(crate) fn query(&self) -> Option<&Query> {
        (!self.query.is_empty()).then_some(&self.query)
    }
}

/// The column a merged table carries: which context each row came from. Read off the
/// `$context` key the merge stamps on the row.
pub static CONTEXT_COLUMN: Column = Column {
    header: "CONTEXT",
    path: "$context",
    kind: nutsh_catalog::ColumnKind::Text,
};

/// `columns`, with [`CONTEXT_COLUMN`] second when the view merges several contexts: after the
/// name, which stays the pinned first column.
pub fn columns_for(
    kind: &'static Kind,
    wide: bool,
    merged: bool,
) -> std::borrow::Cow<'static, [Column]> {
    let base = columns(kind, wide);
    if !merged {
        return std::borrow::Cow::Borrowed(base);
    }
    let mut out = base.to_vec();
    out.insert(1.min(out.len()), CONTEXT_COLUMN);
    std::borrow::Cow::Owned(out)
}

/// The columns a view shows: curated first when any exist, else the fallback columns, capped.
pub fn columns(kind: &'static Kind, wide: bool) -> &'static [Column] {
    let all = if kind.columns.is_empty() {
        kind.fallback_columns
    } else {
        kind.columns
    };
    let n = if wide { WIDE_COLUMNS } else { DEFAULT_COLUMNS };
    &all[..all.len().min(n)]
}

/// How many of a table's rows a filter keeps: what the cursor clamps against and what the
/// title's `n of m` counts. `None` is the whole table, counted without walking it.
pub(crate) fn matching(table: &Table, filter: Option<&Query>) -> usize {
    match filter {
        None => table.rows.len(),
        Some(query) => table.rows.values().filter(|e| query.matches(e)).count(),
    }
}

/// The extId of every row in the order the view shows it: what the filter keeps, in the store's
/// order, or in the rendered text of the sort column with the extId breaking ties. A sort column
/// the view does not show - `w` narrowed the table under it - leaves the order alone rather
/// than indexing off the end.
///
/// The selection indexes this, so every key that acts on "the selected row" and the frame the
/// user is looking at agree on which row that is. The filter is applied here and nowhere else
/// for that reason: a frame that dropped rows the selection still counted would act on the row
/// above or below the one under the cursor.
pub(crate) fn order<'a>(
    table: &'a Table,
    cols: &[Column],
    names: &Names,
    now: SystemTime,
    sort: Option<(usize, bool)>,
    filter: Option<&Query>,
) -> Vec<&'a str> {
    let kept = || {
        table
            .rows
            .iter()
            .filter(move |(_, e)| filter.is_none_or(|q| q.matches(e)))
    };
    let sort_column = sort.and_then(|(col, reverse)| Some((cols.get(col)?, reverse)));
    let Some((col, reverse)) = sort_column else {
        return kept().map(|(ext_id, _)| ext_id.as_str()).collect();
    };
    let mut keyed: Vec<(String, &str)> = kept()
        .map(|(ext_id, e)| (cell::render(col, e, names, now), ext_id.as_str()))
        .collect();
    // Compared in the direction asked for, with the extId breaking ties the same way either
    // way: reversing a sorted list instead would also reverse every group of equal cells, so
    // rows that share a power state would shuffle under the cursor on every `S`.
    keyed.sort_by(|a, b| {
        let cells = if reverse {
            b.0.cmp(&a.0)
        } else {
            a.0.cmp(&b.0)
        };
        cells.then_with(|| a.1.cmp(b.1))
    });
    keyed.into_iter().map(|(_, ext_id)| ext_id).collect()
}

/// Every row's cells, in the table's current order, plus the extId of each row. The `dim` flag
/// travels with the text so the drawer never re-derives what the renderer already decided.
pub(crate) fn cells(
    table: &Table,
    cols: &[Column],
    names: &Names,
    now: SystemTime,
    sort: Option<(usize, bool)>,
    filter: Option<&Query>,
) -> Vec<(String, Vec<cell::Rendered>)> {
    order(table, cols, names, now, sort, filter)
        .into_iter()
        .filter_map(|id| table.rows.get(id).map(|e| (id, e)))
        .map(|(id, e)| {
            (
                id.to_string(),
                cols.iter()
                    .map(|c| cell::render_cell(c, e, names, now))
                    .collect(),
            )
        })
        .collect()
}

/// The selection bar the Table widget reserves on every row, selected or not, and the cells
/// between two columns. [`overhead`] and the widget builder both read them, so the budget the
/// widths are split from cannot drift from what the widget spends.
pub(crate) const GUTTER: &str = "▌ ";
pub(crate) const SPACING: u16 = 2;

/// The cells `ui::MARK` takes in front of a marked row's first cell - see [`marked_rule`].
pub(crate) const MARK_CELLS: u16 = 2;

/// A heading is protected up to this many cells: neither a `Cap` nor a floor falls below it.
/// sofka's headings are `AGE` and `STATUS`; ours are `CREATE TIME` and
/// `VM GUEST CUSTOMIZATION STATUS`. Twelve keeps `CREATE TIME` whole while still refusing to
/// spend twenty-nine columns on a heading whose cells are all `-`.
const HEADER_PROTECT: u16 = 12;

/// The cells [`sort_mark`] takes after the sort column's heading.
pub(crate) const SORT_MARK_CELLS: u16 = 2;

/// What follows the sort column's heading: ` ↑`, or ` ↓` reversed. The heading is cut to leave
/// the mark its cells, so the arrow is the last cell of the heading whatever width the column
/// was given - `VM GUEST CUSTOMIZA ↑` at `Cap(20)`.
pub(crate) fn sort_mark(reverse: bool) -> &'static str {
    if reverse { " ↓" } else { " ↑" }
}

/// What `n` columns cost before any of them gets a cell: the gutter and the spacing between
/// them.
pub(crate) fn overhead(n: u16) -> u16 {
    let gutter = u16::try_from(GUTTER.width()).unwrap_or(u16::MAX);
    gutter.saturating_add(SPACING.saturating_mul(n.saturating_sub(1)))
}

/// The furthest `←`/`→` can scroll: the first column is anchored, and at least one other
/// stays. `saturating_sub`, since `len - 2` underflows on the one-column tables the catalog
/// has several of.
pub(crate) fn max_offset(cols: &[Column]) -> usize {
    cols.len().saturating_sub(2)
}

/// How a column's width is decided when splitting the frame. From sofka
/// (`src/ui.rs:960-1075`, MIT OR Apache-2.0, Copyright (c) 2026 Nikola Milojević), together
/// with `distribute_column_widths` and `share_by_weight` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColWidth {
    /// Honoured exactly. No rule produces one yet - every column is a `Cap` or a `Flex` - but
    /// the waterfall honours it and `width_exact_and_cap_rules` pins that, so it stays for
    /// the curated rule that will want a column nailed to a width.
    #[allow(dead_code)]
    Exact(u16),
    /// Sized to the widest visible value, never above the cap.
    Cap(u16),
    /// Sized to the widest visible value when it fits; surplus and deficit are shared between
    /// Flex columns proportionally to the weight.
    Flex(u16),
}

/// Split `budget` cells across columns. Exact and Cap columns take their width first; each
/// Flex column then gets its full content width whenever its weight-share covers it (a
/// waterfall, so a short NAME frees space for a long CLUSTER), and the final surplus or
/// deficit is shared by weight. Padding is always trimmed before data.
pub(crate) fn distribute_column_widths(budget: u16, cols: &[(ColWidth, u16)]) -> Vec<u16> {
    let mut widths: Vec<u16> = cols
        .iter()
        .map(|(rule, needed)| match rule {
            ColWidth::Exact(w) => *w,
            ColWidth::Cap(cap) => (*needed).min(*cap),
            ColWidth::Flex(_) => 0,
        })
        .collect();
    let fixed: u32 = widths.iter().map(|&w| u32::from(w)).sum();
    let mut left = u32::from(budget).saturating_sub(fixed);

    let flex: Vec<(usize, u32, u32)> = cols
        .iter()
        .enumerate()
        .filter_map(|(i, (rule, needed))| match rule {
            ColWidth::Flex(w) => Some((i, u32::from(*w), u32::from(*needed))),
            _ => None,
        })
        .collect();

    // Waterfall: grant the full content width to any column whose weight-share covers it, then
    // let the freed remainder raise the others' shares.
    let mut unsat = flex.clone();
    loop {
        let total: u32 = unsat.iter().map(|&(_, w, _)| w).sum();
        if total == 0 {
            break;
        }
        let Some(p) = unsat
            .iter()
            .position(|&(_, w, need)| left * w / total >= need)
        else {
            break;
        };
        let (i, _, need) = unsat.swap_remove(p);
        // `need` came from a u16, so this cannot saturate; spelled out rather than cast.
        widths[i] = u16::try_from(need).unwrap_or(u16::MAX);
        left -= need;
    }

    if unsat.is_empty() {
        share_by_weight(left, &flex, &mut widths);
    } else {
        share_by_weight(left, &unsat, &mut widths);
    }
    widths
}

/// Add `left` extra cells proportionally to each column's weight, handing out the
/// integer-division remainder one cell at a time.
fn share_by_weight(mut left: u32, cols: &[(usize, u32, u32)], widths: &mut [u16]) {
    let total: u32 = cols.iter().map(|&(_, w, _)| w).sum();
    if total == 0 {
        return;
    }
    let budget = left;
    for &(i, w, _) in cols {
        let share = budget * w / total;
        widths[i] = widths[i].saturating_add(u16::try_from(share).unwrap_or(u16::MAX));
        left -= share;
    }
    for &(i, _, _) in cols {
        if left == 0 {
            break;
        }
        widths[i] = widths[i].saturating_add(1);
        left -= 1;
    }
}

/// The rule for one column. The first column is the one you read, so it takes the lion's share
/// of a wide window and most of the shared space of a narrow one. A `Cap` never falls below
/// the column's heading - see [`heading_cells`] - so `CREATE TIME` is whole where sofka's
/// `AGE` would have been.
pub(crate) fn rule(col: &Column, first: bool, sorted: bool) -> ColWidth {
    // The anchor, except for an address, which falls through to its own `Cap` below rather
    // than being handed thirty-odd cells of padding: `networking.config.FloatingIp`'s identity
    // is its address. No catalog column has kind `Ip` at index 0 yet. Falling through rather
    // than returning
    // is what gives the address the same `cap.max(heading_cells(…))` floor as every other
    // capped kind, with the fifteen written once.
    if first && col.kind != ColumnKind::Ip {
        return ColWidth::Flex(6);
    }
    // A context name, whole: the column that says which Prism Central a row is from is no
    // use cut to ten cells.
    if col.path == CONTEXT_COLUMN.path {
        return ColWidth::Cap(16.max(heading_cells(col, sorted)));
    }
    let by_kind = match col.kind {
        ColumnKind::Status => ColWidth::Cap(16),
        ColumnKind::Timestamp => ColWidth::Cap(7),
        ColumnKind::Ip => ColWidth::Cap(15),
        ColumnKind::Percent => ColWidth::Cap(5),
        ColumnKind::Micros => ColWidth::Cap(7),
        ColumnKind::Bytes => ColWidth::Cap(10),
        ColumnKind::Duration => ColWidth::Cap(6),
        ColumnKind::Bool => ColWidth::Cap(6),
        ColumnKind::Count => ColWidth::Cap(5),
        ColumnKind::Enum => ColWidth::Cap(20),
        // Was `Flex(2)`, chosen when a `Reference` cell was an eight-character stub. It is a
        // name now, so it takes a cap like every other bounded kind rather than bidding
        // against the name column with a weight nobody re-chose.
        ColumnKind::Reference => ColWidth::Cap(18),
        ColumnKind::Text => ColWidth::Flex(1),
    };
    match by_kind {
        ColWidth::Cap(cap) => ColWidth::Cap(cap.max(heading_cells(col, sorted))),
        other => other,
    }
}

/// The first column's rule when anything in the window is marked. `ui::render_rows` puts the
/// mark into the first cell's text before [`widths`] measures it, so a `Flex` anchor simply
/// grows by those cells - but a capped one would clip instead, and clipping the anchor eats the
/// identity the mark was put in front of. Only an `Ip` anchor is capped, so it is the one this
/// is for.
pub(crate) fn marked_rule(rule: ColWidth, marked: bool) -> ColWidth {
    match rule {
        ColWidth::Cap(cap) if marked => ColWidth::Cap(cap.saturating_add(MARK_CELLS)),
        other => other,
    }
}

fn header_cells(col: &Column) -> u16 {
    u16::try_from(col.header.width()).unwrap_or(u16::MAX)
}

/// The cells a heading is guaranteed: the header up to [`HEADER_PROTECT`], plus the sort mark
/// on the sort column. Both the cap floor and the drop floor are built on it, so the arrow has
/// its cells wherever the heading has its own.
fn heading_cells(col: &Column, sorted: bool) -> u16 {
    let mark = if sorted { SORT_MARK_CELLS } else { 0 };
    header_cells(col).min(HEADER_PROTECT).saturating_add(mark)
}

/// The width below which a column is not a column. The heading term matters: a column whose
/// heading is cut to four cells says nothing about what is under it.
pub(crate) fn floor(col: &Column, first: bool, sorted: bool) -> u16 {
    let heading = heading_cells(col, sorted);
    if first {
        heading.max(8)
    } else {
        heading.max(4)
    }
}

/// Which columns a body `inner_width` cells wide shows: the first column, anchored, then the
/// columns from `offset` on, with the rightmost droppable ones removed until the floors fit.
/// `sort` is the sort column's index in `cols`; its floor carries the mark.
///
/// It is needed because `distribute_column_widths` never trims an `Exact` or a `Cap` column -
/// it subtracts their whole width first and gives the Flex columns `budget - fixed`. When the
/// fixed columns alone exceed the budget that remainder is zero and every Flex column, the
/// name column included, comes out zero cells wide. What it bounds is the floors, not the
/// caps: a `Cap` column is reserved at its content width, which can still overflow a budget
/// the floors fit in, and [`fit`] pays that excess back down to the floors - which it can
/// always do once they fit.
///
/// A `Status` column is never dropped (it is what the row is coloured by) and neither is a
/// `Timestamp` (it is what "is this fresh?" is answered by). The test is on `ColumnKind`, not
/// on the header text, so a second `*Status` property that stayed `Enum` is droppable like any
/// other column.
pub(crate) fn visible(
    cols: &[Column],
    offset: usize,
    sort: Option<usize>,
    inner_width: u16,
) -> Vec<usize> {
    if cols.is_empty() {
        return Vec::new();
    }
    let offset = offset.min(max_offset(cols));
    let mut idx: Vec<usize> = std::iter::once(0).chain((1 + offset)..cols.len()).collect();
    while idx.len() > 1 && !floors_fit(cols, &idx, sort, inner_width) {
        let Some(pos) = idx.iter().rposition(|&i| {
            i != 0 && droppable(cols[i].kind) && cols[i].path != CONTEXT_COLUMN.path
        }) else {
            break;
        };
        idx.remove(pos);
    }
    idx
}

fn droppable(kind: ColumnKind) -> bool {
    !matches!(kind, ColumnKind::Status | ColumnKind::Timestamp)
}

/// The overhead of `idx.len()` columns and every one's floor, against the body's width.
fn floors_fit(cols: &[Column], idx: &[usize], sort: Option<usize>, inner_width: u16) -> bool {
    let n = u16::try_from(idx.len()).unwrap_or(u16::MAX);
    let floors: u16 = idx
        .iter()
        .map(|&i| floor(&cols[i], i == 0, Some(i) == sort))
        .fold(0, u16::saturating_add);
    overhead(n).saturating_add(floors) <= inner_width
}

/// After distributing: any column below its floor takes the difference from the column with
/// the most to spare, then any excess over `budget` is paid back by that same column, so the
/// widget never squeezes a column of its own choosing. Every step moves at least one cell out
/// of a column above its floor, so both loops terminate; when no column has a cell to spare
/// the guard gives up rather than cut into a heading - which [`visible`] makes sure it never
/// has to.
///
/// The first half fires where the drop rule does not: a Flex column whose weight-share lands
/// under its floor while the table as a whole fits. The second fires where the floors fit but
/// the reserves do not: `distribute_column_widths` reserves a `Cap` column at its content
/// width, not at its floor, and at eighty columns the six VM columns' reserves alone exceed
/// the budget.
pub(crate) fn fit(widths: &mut [u16], floors: &[u16], budget: u16) {
    debug_assert_eq!(widths.len(), floors.len());
    while let Some(short) = (0..widths.len()).find(|&i| widths[i] < floors[i]) {
        let Some(fat) = fattest(widths, floors) else {
            return;
        };
        let take = (floors[short] - widths[short]).min(widths[fat] - floors[fat]);
        debug_assert!(take > 0);
        widths[short] += take;
        widths[fat] -= take;
    }
    loop {
        let total: u32 = widths.iter().map(|&w| u32::from(w)).sum();
        let excess = u16::try_from(total.saturating_sub(u32::from(budget))).unwrap_or(u16::MAX);
        if excess == 0 {
            return;
        }
        let Some(fat) = fattest(widths, floors) else {
            return;
        };
        let take = excess.min(widths[fat] - floors[fat]);
        debug_assert!(take > 0);
        widths[fat] -= take;
    }
}

/// The column with the most cells above its floor, if any has one.
fn fattest(widths: &[u16], floors: &[u16]) -> Option<usize> {
    (0..widths.len())
        .filter(|&i| widths[i] > floors[i])
        .max_by_key(|&i| widths[i] - floors[i])
}

/// The column whose word tints the row: the first `Status` column of the shown set.
pub(crate) fn status_index(cols: &[Column]) -> Option<usize> {
    cols.iter().position(|c| c.kind == ColumnKind::Status)
}

/// The widest of the heading and the cells actually on screen, per column. `sort` is the sort
/// column's index in `cols`; its heading is measured with the mark.
///
/// The viewport window rather than a 200-row sample: a page draws four tables at once, and
/// measuring rows nobody is looking at is a frame's budget spent on nothing. Columns can
/// therefore change width as you scroll; in practice the `Cap` columns are pinned by their
/// headers or their caps and only the Flex name column breathes, which is the column you want
/// to breathe.
///
/// Measured in display cells, which is what ratatui lays a column out in: a CJK name counted
/// as characters would be given half the columns it needs and print over its neighbour.
pub(crate) fn widths<S: AsRef<str>>(
    cols: &[Column],
    sort: Option<usize>,
    window: &[Vec<S>],
) -> Vec<u16> {
    cols.iter()
        .enumerate()
        .map(|(i, c)| {
            let widest = window
                .iter()
                .map(|cells| cells.get(i).map_or(0, |c| c.as_ref().width()))
                .max()
                .unwrap_or(0);
            let mark = if Some(i) == sort {
                usize::from(SORT_MARK_CELLS)
            } else {
                0
            };
            // Clamped after the conversion, not before: `as u16` on a cell wider than 65 535
            // wraps, and a column of the low bits of a long value is narrower than the header.
            u16::try_from((c.header.width() + mark).max(widest)).unwrap_or(u16::MAX)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use nutsh_core::store::Table;
    use nutsh_prism::Entity;
    use serde_json::json;

    use super::*;

    /// A column with all three of its fields spelled out; [`col`] and [`kcol`] each fix the
    /// one they do not care about.
    fn column(header: &'static str, path: &'static str, kind: ColumnKind) -> Column {
        Column { header, path, kind }
    }

    fn col(header: &'static str, path: &'static str) -> Column {
        column(header, path, ColumnKind::Text)
    }

    fn vm() -> &'static Kind {
        nutsh_catalog::kind("vmm.ahv.config.Vm").expect("the catalog has VMs")
    }

    /// A table of entities built straight from JSON, so `cells` can be exercised without a
    /// server.
    fn table(rows: &[serde_json::Value]) -> Table {
        let mut t = Table::default();
        for raw in rows {
            let e = Entity::new(vm(), raw.clone(), None);
            t.rows.insert(e.ext_id.clone(), e);
        }
        t
    }

    /// The six VM fallback columns: the design's worked example for the width rules, whose
    /// long `Enum` headings and `Timestamp` the curated VM columns no longer carry.
    fn vm_fallback() -> &'static [Column] {
        &vm().fallback_columns[..DEFAULT_COLUMNS]
    }

    #[test]
    fn columns_are_capped_narrow_and_wide() {
        // VMs are curated: nine columns, of which six show by default and all nine under `w`.
        assert_eq!(vm().columns.len(), 9);
        assert_eq!(columns(vm(), false).len(), DEFAULT_COLUMNS);
        assert_eq!(columns(vm(), false)[0].header, "NAME");
        assert_eq!(columns(vm(), true).len(), 9);
        // An uncurated kind with a long schema is where the twenty-column cap bites.
        let wide = nutsh_catalog::KINDS
            .iter()
            .find(|k| k.columns.is_empty() && k.fallback_columns.len() >= WIDE_COLUMNS)
            .expect("the catalog has uncurated kinds with twenty fallback columns");
        assert_eq!(columns(wide, false).len(), DEFAULT_COLUMNS);
        assert_eq!(columns(wide, true).len(), WIDE_COLUMNS);
    }

    /// A kind with fewer columns than the cap keeps all of them rather than panicking on the
    /// slice.
    #[test]
    fn columns_of_a_short_kind_are_all_of_them() {
        let short = nutsh_catalog::KINDS
            .iter()
            .find(|k| k.fallback_columns.len() < DEFAULT_COLUMNS && k.columns.is_empty())
            .expect("the catalog has kinds with fewer than six columns");
        assert_eq!(columns(short, false).len(), short.fallback_columns.len());
        assert_eq!(columns(short, true).len(), short.fallback_columns.len());
    }

    fn kcol(header: &'static str, kind: ColumnKind) -> Column {
        column(header, "x", kind)
    }

    /// sofka's four width tests, transcribed: the waterfall trims padding before data, spreads
    /// a surplus by weight, falls back to weight shares in a hard deficit, and honours Exact
    /// and Cap.
    #[test]
    fn width_deficit_trims_padding_before_data() {
        let cols = [
            (ColWidth::Flex(6), 10),
            (ColWidth::Flex(1), 15),
            (ColWidth::Cap(7), 3),
        ];
        assert_eq!(distribute_column_widths(28, &cols), vec![10, 15, 3]);
    }

    #[test]
    fn width_surplus_spreads_by_weight() {
        let cols = [(ColWidth::Flex(6), 10), (ColWidth::Flex(2), 5)];
        let widths = distribute_column_widths(55, &cols);
        assert_eq!(widths, vec![40, 15]);
        assert_eq!(widths.iter().map(|&w| u32::from(w)).sum::<u32>(), 55);
    }

    #[test]
    fn width_hard_deficit_shares_by_weight() {
        let cols = [(ColWidth::Flex(6), 100), (ColWidth::Flex(1), 100)];
        assert_eq!(distribute_column_widths(21, &cols), vec![18, 3]);
    }

    #[test]
    fn width_exact_and_cap_rules() {
        let cols = [
            (ColWidth::Exact(12), 3),
            (ColWidth::Cap(19), 7),
            (ColWidth::Cap(19), 25),
            (ColWidth::Flex(1), 5),
        ];
        let widths = distribute_column_widths(60, &cols);
        assert_eq!(widths, vec![12, 7, 19, 22]);
    }

    /// Deviation 1: our headings are `CREATE TIME`, not `AGE`. A cap below the heading would
    /// print a heading nobody can read, so a `Cap` is floored at the header up to twelve cells
    /// - and no further, so a `VM GUEST CUSTOMIZATION STATUS` column of `-` still gets twenty.
    #[test]
    fn a_cap_never_falls_below_its_own_heading() {
        let create_time = kcol("CREATE TIME", ColumnKind::Timestamp);
        let long = kcol("VM GUEST CUSTOMIZATION STATUS", ColumnKind::Enum);
        assert_eq!(rule(&create_time, false, false), ColWidth::Cap(11));
        assert_eq!(
            rule(&kcol("AGE", ColumnKind::Timestamp), false, false),
            ColWidth::Cap(7)
        );
        assert_eq!(rule(&long, false, false), ColWidth::Cap(20));
        assert_eq!(
            rule(&kcol("NAME", ColumnKind::Text), true, false),
            ColWidth::Flex(6)
        );
        // The sort column's heading carries ` ↑`: two more cells on a protected heading, and
        // none on one already cut by its cap, where the mark takes the heading's last cells.
        assert_eq!(rule(&create_time, false, true), ColWidth::Cap(13));
        assert_eq!(rule(&long, false, true), ColWidth::Cap(20));
        assert_eq!(floor(&create_time, false, true), 13);
        assert_eq!(
            floor(&long, false, true),
            14,
            "the protected twelve plus the mark"
        );
        assert_eq!(floor(&kcol("N", ColumnKind::Text), true, true), 8);
    }

    #[test]
    fn the_sort_mark_is_two_cells() {
        for reverse in [false, true] {
            assert_eq!(
                sort_mark(reverse).width(),
                usize::from(SORT_MARK_CELLS),
                "{reverse}"
            );
        }
        assert_eq!(overhead(1), 2, "the gutter alone");
        assert_eq!(overhead(6), 12, "the gutter and five gaps");
        assert_eq!(overhead(0), 2);
    }

    #[test]
    fn every_column_kind_has_a_rule() {
        for (kind, want) in [
            (ColumnKind::Status, ColWidth::Cap(16)),
            (ColumnKind::Timestamp, ColWidth::Cap(7)),
            (ColumnKind::Ip, ColWidth::Cap(15)),
            (ColumnKind::Percent, ColWidth::Cap(5)),
            (ColumnKind::Micros, ColWidth::Cap(7)),
            (ColumnKind::Bytes, ColWidth::Cap(10)),
            (ColumnKind::Duration, ColWidth::Cap(6)),
            (ColumnKind::Bool, ColWidth::Cap(6)),
            (ColumnKind::Count, ColWidth::Cap(5)),
            (ColumnKind::Enum, ColWidth::Cap(20)),
            (ColumnKind::Reference, ColWidth::Cap(18)),
            (ColumnKind::Text, ColWidth::Flex(1)),
        ] {
            assert_eq!(rule(&kcol("H", kind), false, false), want, "{kind:?}");
        }
    }

    /// A `Reference` cell is a name of seven to twenty cells rather than an eight-character
    /// stub, and `Flex(2)` - a weight chosen for stubs - lets it bid against the name column.
    #[test]
    fn a_reference_is_capped_not_flexed() {
        assert_eq!(
            rule(&kcol("CLUSTER", ColumnKind::Reference), false, false),
            ColWidth::Cap(18)
        );
    }

    /// `rule` returns `Flex(6)` for column 0 before it looks at the kind, so a `Cap(15)` could
    /// never reach a first column. `networking.config.FloatingIp`'s identity *is* its address,
    /// and a 15-cell value in a `Flex(6)` first column would be handed thirty-odd cells of
    /// padding. No catalog column reaches the branch yet.
    ///
    /// The usual worry about a `Cap` first column - that the table then has no `Flex` column
    /// to absorb the surplus and stops short of the pane - does not arise there: `NAME` is the
    /// sixth column, `Text`, therefore `Flex(1)`, so it takes what the addresses do not.
    #[test]
    fn an_address_in_the_first_column_is_still_capped() {
        assert_eq!(
            rule(&kcol("IP", ColumnKind::Ip), true, false),
            ColWidth::Cap(15)
        );
        // Every other kind keeps the anchor.
        assert_eq!(
            rule(&kcol("NAME", ColumnKind::Text), true, false),
            ColWidth::Flex(6)
        );
        assert_eq!(
            rule(&kcol("CLUSTER", ColumnKind::Reference), true, false),
            ColWidth::Flex(6)
        );
    }

    /// A capped anchor is the one that has to make room for `space`'s mark: `ui::render_rows`
    /// puts it into the first cell's text before `widths` measures it, so `Cap(15)` would clip
    /// `* 255.255.255.255` back to `* 255.255.255.2` - the identity, eaten by the glyph that
    /// marked it. A `Flex` anchor needs nothing; it grows.
    #[test]
    fn a_marked_capped_anchor_keeps_room_for_the_mark() {
        assert_eq!(marked_rule(ColWidth::Cap(15), true), ColWidth::Cap(17));
        assert_eq!(marked_rule(ColWidth::Cap(15), false), ColWidth::Cap(15));
        assert_eq!(marked_rule(ColWidth::Flex(6), true), ColWidth::Flex(6));
    }

    /// Deviation 2, the drop rule, on the worked example of the design: the six VM fallback
    /// columns in the 76-cell body of a 100-column frame. Floors are 8 + 12 + 11 + 12 + 12 + 11
    /// = 66, plus 2 for the gutter and 10 for the spacing = 78 > 74, so exactly one column
    /// goes - the rightmost that is neither the first, nor a `Status`, nor a `Timestamp`.
    #[test]
    fn the_drop_rule_removes_the_rightmost_droppable_column() {
        let vm_cols = vm_fallback();
        assert_eq!(vm_cols.len(), 6);
        let kept = visible(vm_cols, 0, None, 74);
        assert_eq!(
            kept,
            vec![0, 1, 2, 3, 5],
            "VM GUEST CUSTOMIZATION STATUS goes"
        );
        assert_eq!(
            visible(vm_cols, 0, None, 120),
            vec![0, 1, 2, 3, 4, 5],
            "all six fit"
        );
        // Narrower still: the next droppable from the right is PROTECTION TYPE, then MACHINE
        // TYPE; POWER STATE (Status) and CREATE TIME (Timestamp) are never dropped.
        let narrow = visible(vm_cols, 0, None, 40);
        assert!(
            narrow.contains(&0) && narrow.contains(&2) && narrow.contains(&5),
            "{narrow:?}"
        );
        assert!(!narrow.contains(&4), "{narrow:?}");
    }

    /// The sort column's floor carries its mark, and the drop rule counts it: the six floors
    /// plus the overhead are exactly the 78 cells of an 80-column frame, and sorting on
    /// CREATE TIME asks for two more.
    #[test]
    fn the_drop_rule_counts_the_sort_mark() {
        let vm_cols = vm_fallback();
        assert_eq!(visible(vm_cols, 0, None, 78), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(visible(vm_cols, 0, Some(5), 78), vec![0, 1, 2, 3, 5]);
        assert_eq!(visible(vm_cols, 0, Some(5), 80), vec![0, 1, 2, 3, 4, 5]);
    }

    /// The first column is anchored and the rest scroll under it; the offset is clamped to
    /// `len - 2`, which would underflow on the one-column tables the catalog has several of.
    #[test]
    fn horizontal_scroll_anchors_the_first_column_and_clamps() {
        let vm_cols = columns(vm(), false);
        assert_eq!(visible(vm_cols, 2, None, 120), vec![0, 3, 4, 5]);
        assert_eq!(max_offset(vm_cols), 4);
        assert_eq!(
            visible(vm_cols, 99, None, 120),
            vec![0, 5],
            "clamped to len - 2"
        );
        let one = [kcol("NAME", ColumnKind::Text)];
        assert_eq!(max_offset(&one), 0);
        assert_eq!(visible(&one, 3, None, 120), vec![0], "no underflow");
        assert_eq!(max_offset(&[]), 0);
        assert_eq!(visible(&[], 0, None, 120), Vec::<usize>::new());
    }

    /// Deviation 3: a Flex column whose weight-share lands under its floor takes the
    /// difference from the column with the most to spare. The total is conserved.
    #[test]
    fn the_starvation_guard_conserves_the_total() {
        let mut widths = vec![2, 30, 8];
        let floors = [8, 4, 4];
        fit(&mut widths, &floors, 40);
        assert_eq!(widths, vec![8, 24, 8]);
        assert_eq!(widths.iter().sum::<u16>(), 40, "conserved");
        // Nothing to take: the guard gives up rather than looping.
        let mut tight = vec![1, 1];
        fit(&mut tight, &[4, 4], 2);
        assert_eq!(tight, vec![1, 1]);
    }

    /// Deviation 3, the other half: the floors fit but the reserves do not. The six VM
    /// fallback columns in the 78-cell body of an 80-column frame: floors 66 + overhead 12 =
    /// 78, so nothing is dropped, but a `Cap` column is reserved at its content width, and
    /// 12 + 11 + 15 + 20 + 11 = 69 > 66 leaves NAME nothing. The guard lifts NAME to its
    /// floor, then pays the three cells of excess back from the column with the most to
    /// spare, so the total is the budget and ratatui has nothing to squeeze.
    #[test]
    fn the_guard_pays_the_excess_back_down_to_the_floors() {
        let vm_cols = vm_fallback();
        assert_eq!(visible(vm_cols, 0, None, 78), vec![0, 1, 2, 3, 4, 5]);
        let window = vec![vec!["web-01", "PC", "ON", "-", "-", "127d"]];
        let needed = widths(vm_cols, None, &window);
        assert_eq!(
            needed,
            vec![6, 12, 11, 15, 29, 11],
            "the headings, and NAME"
        );
        let rules: Vec<(ColWidth, u16)> = vm_cols
            .iter()
            .enumerate()
            .map(|(i, c)| (rule(c, i == 0, false), needed[i]))
            .collect();
        let budget = 78 - overhead(6);
        assert_eq!(budget, 66);
        let mut got = distribute_column_widths(budget, &rules);
        assert_eq!(got, vec![0, 12, 11, 15, 20, 11], "NAME is starved");
        let floors: Vec<u16> = vm_cols
            .iter()
            .enumerate()
            .map(|(i, c)| floor(c, i == 0, false))
            .collect();
        assert_eq!(floors, vec![8, 12, 11, 12, 12, 11]);
        fit(&mut got, &floors, budget);
        assert_eq!(got, vec![8, 12, 11, 12, 12, 11]);
        assert_eq!(got.iter().sum::<u16>(), budget, "nothing left to squeeze");
    }

    #[test]
    fn the_first_status_column_decides_the_row() {
        let cols = [
            kcol("NAME", ColumnKind::Text),
            kcol("POWER", ColumnKind::Status),
            kcol("SYNC", ColumnKind::Status),
        ];
        assert_eq!(status_index(&cols), Some(1));
        assert_eq!(status_index(&cols[..1]), None);
    }

    /// What is measured: the header, or the widest cell of the window, whichever is wider.
    #[test]
    fn widths_use_the_header_when_the_cells_are_narrower() {
        let cols = [col("MACHINE TYPE", "machineType"), col("N", "n")];
        let window = vec![
            vec!["PC".to_string(), "12345".to_string()],
            vec!["PC".to_string(), "7".to_string()],
        ];
        // Column 0: the header wins over "PC"; column 1: the widest cell wins over "N".
        assert_eq!(widths(&cols, None, &window), vec![12, 5]);
    }

    /// The sort column's heading is measured with its ` ↑`, so a `Cap` pinned by its heading
    /// has the arrow's cells; a column whose cells are wider than that is unchanged.
    #[test]
    fn widths_measure_the_sort_mark_on_the_sort_column() {
        let cols = [col("MACHINE TYPE", "machineType"), col("N", "n")];
        let window = vec![vec!["PC".to_string(), "12345".to_string()]];
        assert_eq!(widths(&cols, Some(0), &window), vec![14, 5]);
        assert_eq!(widths(&cols, Some(1), &window), vec![12, 5]);
    }

    /// A cell wider than `u16::MAX` must not wrap around it: 65 546 characters becoming 10
    /// leaves the column narrower than its own heading. It saturates instead, and the Flex
    /// share decides what such a column really gets.
    #[test]
    fn widths_saturate_on_a_cell_too_wide_to_count() {
        let cols = [col("NAME", "name")];
        let window = vec![vec!["x".repeat(65_546)]];
        assert_eq!(widths(&cols, None, &window), vec![u16::MAX]);
    }

    /// Widths are the columns the terminal draws, not the characters behind them: a CJK name
    /// takes two cells per character, and counting characters would give it half the room it
    /// needs and let it print over the column beside it.
    #[test]
    fn widths_count_display_cells() {
        let cols = [col("NAME", "name")];
        let window = vec![vec!["東京都".to_string()]];
        assert_eq!(widths(&cols, None, &window), vec![6]);
    }

    /// A row shorter than the column list contributes nothing rather than panicking.
    #[test]
    fn widths_tolerate_a_short_row() {
        let cols = [col("NAME", "name"), col("POWER", "powerState")];
        let window = vec![vec!["web-01".to_string()]];
        assert_eq!(widths(&cols, None, &window), vec![6, 5]);
    }

    /// The flag travels with the text, so the drawer never re-derives it.
    #[test]
    fn cells_carry_the_dim_flag() {
        let cols = [
            column("NAME", "name", ColumnKind::Text),
            column("CLUSTER", "cluster.extId", ColumnKind::Reference),
            column("CREATED", "createTime", ColumnKind::Timestamp),
        ];
        let t = table(&[json!({
            "extId": "3d0c4a2e-1b8f-4c1a-9e2f-000000000001",
            "name": "web-01",
            "cluster": {"extId": "0006158a-2f0d-4d5a-8e2d-000000000010"},
        })]);
        let rows = cells(
            &t,
            &cols,
            &Names::default(),
            SystemTime::UNIX_EPOCH,
            None,
            None,
        );
        let row = &rows[0].1;
        assert_eq!(row[0].text, "web-01");
        assert!(!row[0].dim, "a name is not dim");
        assert_eq!(row[1].text, "0006158a");
        assert!(row[1].dim, "an unresolved reference is dim");
        assert_eq!(row[2].text, "-");
        assert!(row[2].dim, "an empty cell is dim");
    }

    #[test]
    fn cells_keep_store_order_until_sorted() {
        let t = table(&[
            json!({"extId": "1", "name": "web-01"}),
            json!({"extId": "2", "name": "db-01"}),
        ]);
        let cols = [col("NAME", "name")];
        let rows = cells(
            &t,
            &cols,
            &Names::default(),
            SystemTime::UNIX_EPOCH,
            None,
            None,
        );
        assert_eq!(names(&rows), ["web-01", "db-01"]);
    }

    #[test]
    fn cells_sort_by_the_chosen_column_and_reverse() {
        let t = table(&[
            json!({"extId": "1", "name": "web-01", "powerState": "ON"}),
            json!({"extId": "2", "name": "db-01", "powerState": "OFF"}),
            json!({"extId": "3", "name": "web-02", "powerState": "ON"}),
        ]);
        let cols = [col("NAME", "name"), col("POWER STATE", "powerState")];
        let now = SystemTime::UNIX_EPOCH;
        let by_name = cells(&t, &cols, &Names::default(), now, Some((0, false)), None);
        assert_eq!(names(&by_name), ["db-01", "web-01", "web-02"]);
        let reversed = cells(&t, &cols, &Names::default(), now, Some((0, true)), None);
        assert_eq!(names(&reversed), ["web-02", "web-01", "db-01"]);
        // A second column sorts by that column's text: OFF before ON.
        let by_power = cells(&t, &cols, &Names::default(), now, Some((1, false)), None);
        assert_eq!(by_power[0].1[1].text, "OFF");
    }

    /// Rows whose sort cell is equal keep the same order in both directions: only the cells
    /// decide, and the extId decides the rest. Reversing a sorted list would flip the ties
    /// too, and every `S` would shuffle rows that look identical in that column.
    #[test]
    fn ties_keep_their_order_when_the_sort_is_reversed() {
        let t = table(&[
            json!({"extId": "1", "name": "web-01", "powerState": "ON"}),
            json!({"extId": "2", "name": "db-01", "powerState": "OFF"}),
            json!({"extId": "3", "name": "web-02", "powerState": "ON"}),
        ]);
        let cols = [col("NAME", "name"), col("POWER STATE", "powerState")];
        let now = SystemTime::UNIX_EPOCH;
        let up = cells(&t, &cols, &Names::default(), now, Some((1, false)), None);
        assert_eq!(names(&up), ["db-01", "web-01", "web-02"]);
        let down = cells(&t, &cols, &Names::default(), now, Some((1, true)), None);
        assert_eq!(names(&down), ["web-01", "web-02", "db-01"]);
    }

    /// A sort column beyond the visible ones (a `wide` toggle that narrowed the table, say)
    /// leaves the order alone rather than indexing off the end.
    #[test]
    fn cells_ignore_a_sort_column_that_is_not_shown() {
        let t = table(&[
            json!({"extId": "1", "name": "web-01"}),
            json!({"extId": "2", "name": "db-01"}),
        ]);
        let cols = [col("NAME", "name")];
        let rows = cells(
            &t,
            &cols,
            &Names::default(),
            SystemTime::UNIX_EPOCH,
            Some((7, false)),
            None,
        );
        assert_eq!(names(&rows), ["web-01", "db-01"]);
    }

    /// `order` is what the selection indexes and `cells` is what the frame draws; a sort that
    /// moved one and not the other would open the row above or below the one on screen.
    #[test]
    fn order_and_cells_agree_under_a_sort() {
        let t = table(&[
            json!({"extId": "1", "name": "web-01"}),
            json!({"extId": "2", "name": "db-01"}),
            json!({"extId": "3", "name": "web-02"}),
        ]);
        let cols = [col("NAME", "name")];
        let now = SystemTime::UNIX_EPOCH;
        for sort in [None, Some((0, false)), Some((0, true)), Some((7, false))] {
            let ids = order(&t, &cols, &Names::default(), now, sort, None);
            let rows = cells(&t, &cols, &Names::default(), now, sort, None);
            assert_eq!(
                ids,
                rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
                "{sort:?}"
            );
        }
    }

    /// The three readings of a filtered table have to agree: the rows the frame draws, the
    /// order the selection indexes, and the `n` the title counts. Two of them disagreeing is a
    /// cursor standing on a row nobody can see.
    #[test]
    fn a_filter_narrows_the_order_the_cells_and_the_count_together() {
        let t = table(&[
            json!({"extId": "1", "name": "web-01"}),
            json!({"extId": "2", "name": "db-01"}),
            json!({"extId": "3", "name": "web-02"}),
        ]);
        let cols = [col("NAME", "name")];
        let now = SystemTime::UNIX_EPOCH;
        let web = Query::new("web");
        for sort in [None, Some((0, true))] {
            let ids = order(&t, &cols, &Names::default(), now, sort, Some(&web));
            let rows = cells(&t, &cols, &Names::default(), now, sort, Some(&web));
            assert_eq!(ids.len(), 2, "{sort:?}");
            assert_eq!(matching(&t, Some(&web)), ids.len(), "{sort:?}");
            assert_eq!(
                ids,
                rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
                "{sort:?}"
            );
        }
        assert_eq!(matching(&t, None), 3, "no filter is the whole table");
        assert_eq!(matching(&t, Some(&Query::new("nothing"))), 0);
    }

    /// A bare `/` is not a filter: the term is empty, so the view keeps every row until a
    /// letter is typed, and `esc` is never needed to undo a keystroke that did nothing.
    #[test]
    fn an_empty_term_is_not_yet_a_filter() {
        let mut filter = Filter::new();
        assert!(filter.typing() && filter.query().is_none());
        assert_eq!(filter.key(Key::Char('w')), Some(Edit::Changed));
        assert_eq!(filter.term(), "w");
        assert_eq!(filter.query().map(Query::text), Some("w"));
        // The one mutator re-parses, so the term on screen and the rows under it cannot drift.
        assert_eq!(filter.key(Key::Backspace), Some(Edit::Changed));
        assert!(filter.query().is_none(), "back to no filter");
        assert_eq!(filter.key(Key::Enter), None, "the view answers for it");
        filter.commit();
        assert!(!filter.typing(), "and the keys go back to the table");
    }

    fn names(rows: &[(String, Vec<cell::Rendered>)]) -> Vec<&str> {
        rows.iter().map(|(_, c)| c[0].text.as_str()).collect()
    }
}
