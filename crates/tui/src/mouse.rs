//! Input the app understands from a pointer, independent of crossterm so tests need no
//! terminal - the rule `key.rs` follows - plus the hit map `ui::draw` records as it draws.

use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Click,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
}

/// What the last frame laid out. **Recorded** during `ui::draw`, never replayed: mapping a click
/// back to a row means knowing the table's area, its scroll offset and the x span of each
/// column *as this frame drew them*, and those numbers are computed inside `ui::render_rows` by
/// six calls that must agree with each other. Recomputing them outside the draw would be a
/// second copy that would silently stop agreeing the first time one of them changed.
///
/// There is precedent in the same file: `ui::draw` already writes the frame width back into the
/// app with `app.width.set(area.width)`, for exactly this reason.
#[derive(Default)]
pub struct Hits {
    pub sidebar: Option<ListHit>,
    /// The table view, or one entry per page pane that drew rows, in draw order.
    pub tables: Vec<TableHit>,
    /// A modal's list, when one is open: everything outside it is inert.
    pub modal: Option<ListHit>,
    /// The header's version line: a click here opens the settings screen.
    pub version: Option<Rect>,
}

pub struct ListHit {
    pub rows: Rect,
    pub offset: usize,
    pub len: usize,
}

pub struct TableHit {
    /// `None` for the table view; the pane's index in `PageView::panes` otherwise. Not the
    /// entry's own index in [`Hits::tables`]: a pane that says why it has no rows draws no
    /// table at all, so the two part company on the first page with one.
    pub pane: Option<usize>,
    /// The whole bordered area: a click here focuses the pane.
    pub block: Rect,
    /// One row.
    pub header: Rect,
    pub rows: Rect,
    pub offset: usize,
    pub len: usize,
    /// `(x, width, index into the view's full column list)` for each drawn column. The index is
    /// `shown[i]` and not `i`, so the column scroll and the drop rule come out right for free.
    pub columns: Vec<(u16, u16, usize)>,
}

/// Whether `m` is inside `r`.
pub fn contains(r: Rect, m: Mouse) -> bool {
    m.col >= r.x
        && m.col < r.x.saturating_add(r.width)
        && m.row >= r.y
        && m.row < r.y.saturating_add(r.height)
}

/// The index of the entry under `m`, or `None` outside the rows and past the last one - a click
/// in the empty space below the last row selects nothing.
fn index_in(rows: Rect, offset: usize, len: usize, m: Mouse) -> Option<usize> {
    if !contains(rows, m) {
        return None;
    }
    let i = offset + usize::from(m.row - rows.y);
    (i < len).then_some(i)
}

impl ListHit {
    pub fn index_at(&self, m: Mouse) -> Option<usize> {
        index_in(self.rows, self.offset, self.len, m)
    }
}

impl TableHit {
    pub fn index_at(&self, m: Mouse) -> Option<usize> {
        index_in(self.rows, self.offset, self.len, m)
    }

    /// The index into the view's full column list of the column under `m`.
    pub fn column_at(&self, m: Mouse) -> Option<usize> {
        self.columns
            .iter()
            .find(|(x, w, _)| m.col >= *x && m.col < x.saturating_add(*w))
            .map(|(_, _, i)| *i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(col: u16, row: u16) -> Mouse {
        Mouse {
            kind: MouseKind::Click,
            col,
            row,
        }
    }

    #[test]
    fn a_row_index_is_the_offset_plus_the_distance_from_the_top() {
        let hit = ListHit {
            rows: Rect {
                x: 2,
                y: 5,
                width: 20,
                height: 4,
            },
            offset: 10,
            len: 14,
        };
        assert_eq!(hit.index_at(at(3, 5)), Some(10));
        assert_eq!(hit.index_at(at(3, 7)), Some(12));
        assert_eq!(hit.index_at(at(3, 8)), Some(13), "the last entry");
        assert_eq!(hit.index_at(at(3, 9)), None, "below the pane");
        assert_eq!(hit.index_at(at(3, 4)), None, "above it");
        assert_eq!(hit.index_at(at(1, 6)), None, "left of it");
        // Four rows of pane, four entries left: the fourth is past `len`.
        let short = ListHit {
            rows: hit.rows,
            offset: 12,
            len: 14,
        };
        assert_eq!(
            short.index_at(at(3, 7)),
            None,
            "the empty space below the last row"
        );
    }

    #[test]
    fn a_column_is_found_by_its_span_and_named_by_its_place_in_the_full_list() {
        let hit = TableHit {
            pane: None,
            block: Rect {
                x: 0,
                y: 0,
                width: 40,
                height: 10,
            },
            header: Rect {
                x: 1,
                y: 1,
                width: 38,
                height: 1,
            },
            rows: Rect {
                x: 1,
                y: 2,
                width: 38,
                height: 7,
            },
            offset: 0,
            len: 3,
            // The first column is anchored and the scroll starts at the third, so the drawn
            // columns are 0, 2 and 3 of the view's full list.
            columns: vec![(3, 10, 0), (15, 6, 2), (23, 6, 3)],
        };
        assert_eq!(hit.column_at(at(3, 1)), Some(0));
        assert_eq!(hit.column_at(at(12, 1)), Some(0));
        assert_eq!(hit.column_at(at(13, 1)), None, "the gap between columns");
        assert_eq!(hit.column_at(at(16, 1)), Some(2));
        assert_eq!(hit.column_at(at(24, 1)), Some(3));
        assert_eq!(hit.column_at(at(39, 1)), None);
    }
}
