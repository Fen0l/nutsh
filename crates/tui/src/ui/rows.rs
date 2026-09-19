//! The table body: rows, marks and the sync badge.

use super::*;

pub(super) fn draw_table(app: &App, f: &mut Frame, area: Rect) {
    let (Some(live), Some(view)) = (&app.live, app.view()) else {
        return;
    };
    let merged = live.merged(&view.key);
    let t = &*merged;
    let columns = table::columns_for(view.key.kind, view.wide, app.merges(&view.key));
    let filter = view.query();
    let mut title = vec![
        Span::styled(format!(" {} ", view.key.kind.display), theme::title()),
        Span::styled(
            format!("[{}]", count(t, filter)),
            Style::default().fg(theme::counter()),
        ),
    ];
    // The term itself, in the colour the prompt line draws the `/` that typed it, so the two
    // read as one gesture. Beside the count and never instead of it: `[12 of 847]` says the
    // table is narrowed and this says what narrowed it.
    if let Some(f) = view.filter.as_ref().filter(|f| !f.term().is_empty()) {
        title.push(Span::styled(
            format!(" /{}", f.term()),
            Style::default().fg(theme::mauve()),
        ));
    }
    title.extend(walking(t));
    // A 404 on the list path is not an error banner in the status line: it is the whole answer,
    // and it belongs where the rows would have been. Indented to where a row would start, the
    // way `draw_pane` indents its own reason - and gated the way `draw_pane` gates it, on
    // having no rows to draw instead: the store's contract for a failed cycle is that the rows
    // stay, and a table restored from the cache whose first live cycle 404s must not lose the
    // rows it is showing (nothing would ever bring them back - the subscription has stopped).
    // The flash at the foot of the frame carries the error for that case.
    if t.not_served && t.rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{}{NOT_SERVED}", " ".repeat(table::GUTTER.width())),
                theme::dim(),
            )))
            .block(table_block(title, app.body_focused())),
            area,
        );
        return;
    }
    render_rows(
        app,
        live,
        Rows {
            key: &view.key,
            pane: None,
            all: &columns,
            col_offset: view.col_offset,
            sort: view.sort,
            selected: view.selected,
            marks: Some(&view.marks),
            cursor: true,
            focused: app.body_focused(),
            // A table view's walk carries the kind's own budget: `Subscription::list` takes it
            // from the kind and nothing narrows it to a height, the way a pane's `sized` does.
            //
            // A term that matches nothing answers before the walk does: rows arrived, and the
            // reason none of them are drawn is the term, not the server. `body_note` speaks for
            // a table with no rows at all, and this speaks for a table whose rows are all
            // filtered out - two different sentences for two different silences.
            note: match filter {
                Some(query) if !t.rows.is_empty() && table::matching(t, filter) == 0 => {
                    Some(Cow::Owned(format!("no row here matches {}", query.text())))
                }
                _ => body_note(t, view.key.kind.max_rows, view.stopped, "no rows"),
            },
            filter,
            title,
        },
        f,
        area,
    );
}

/// The mark `space` puts in front of a marked row's first cell.
pub(super) const MARK: &str = "* ";

/// The rows of one table, in the table chrome: the drop rule, the width waterfall, the row
/// tints and the selection bar. The table view and every page pane go through it, so a pane
/// row is a table row in the strongest sense - the same function drew it.
pub(super) fn render_rows(app: &App, live: &Live, spec: Rows<'_>, f: &mut Frame, area: Rect) {
    // A pane draws its own table; the table view draws every joined context's.
    let merged = if spec.pane.is_none() {
        live.merged(spec.key)
    } else {
        std::borrow::Cow::Borrowed(live.store.table(spec.key))
    };
    let t = &*merged;
    let all = spec.all;
    let mut rows = table::cells(t, all, live.store.names(), app.now, spec.sort, spec.filter);
    // A table with nothing to draw says what is happening instead - `body_note` decided that
    // there is nothing, from the same rows `table::cells` renders. Drawn *after* the widget
    // rather than as a row of it, because a row is cut to its own column's width and
    // `listing up to 500 rows…` is wider than any first column, and because the column headings
    // above it must stay: the frame that says the walk has started is the same frame that says
    // which columns are coming.
    let note = spec.note;
    // The mark goes into the first cell, not the gutter: ratatui gives a table one highlight
    // symbol, and that one is the cursor. A text snapshot then shows the marks without a style.
    //
    // It is inserted before `widths` measures the window, so the first column asks for two
    // more cells while anything in it is marked, and a narrow frame with long names will move
    // the columns to its right by up to two. That is the trade: the alternative is measuring
    // the unmarked cells and cutting two characters off the name of every row the user just
    // marked, which is the row they are looking at. `table::visible` pins column 0, so the
    // mark itself is never scrolled off.
    //
    // A `Flex` anchor simply grows by those two cells. A capped one cannot - only an `Ip`
    // anchor is capped, and clipping is exactly what would eat the address the mark was put in
    // front of - so `marked` feeds `table::marked_rule` below.
    let mut marked = false;
    if let Some(marks) = spec.marks {
        for (ext_id, cells) in &mut rows {
            if marks.contains(ext_id)
                && let Some(first) = cells.first_mut()
            {
                first.text.insert_str(0, MARK);
                // The flag belongs to the value, not to the mark: on a table whose anchor is a
                // reference no name ever resolves, an inherited `dim` would draw the one glyph
                // the user just placed as the absence of a value.
                first.dim = false;
                marked = true;
            }
        }
    }
    // The rows actually on screen: two for the border, one for the header row.
    let height = usize::from(area.height.saturating_sub(3));
    let (offset, window) = crate::palette::window(&rows, spec.selected, height);

    let inner = area.width.saturating_sub(2);
    let sort = spec.sort.map(|(col, _)| col);
    let col_offset = spec.col_offset.min(table::max_offset(all));
    let shown = table::visible(all, col_offset, sort, inner);
    let cols: Vec<Column> = shown.iter().map(|&i| all[i]).collect();
    // The sort column's place among the shown ones - `w` can narrow the table under it, and
    // `→` can scroll it off the left edge.
    let sorted = shown.iter().position(|&i| Some(i) == sort);
    // Borrowed from `rows`, which outlives the render: the cells are allocated once, by
    // `table::cells`, and measured and drawn from there. `table::widths` measures anything that
    // is `AsRef<str>`, and a `Rendered` is.
    let cells: Vec<Vec<&Rendered>> = window
        .iter()
        .map(|(_, row)| shown.iter().map(|&i| &row[i]).collect())
        .collect();

    let needed = table::widths(&cols, sorted, &cells);
    let rules: Vec<(table::ColWidth, u16)> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let rule = table::rule(c, i == 0, Some(i) == sorted);
            (table::marked_rule(rule, marked && i == 0), needed[i])
        })
        .collect();
    let n = u16::try_from(cols.len()).unwrap_or(u16::MAX);
    let budget = inner.saturating_sub(table::overhead(n));
    let mut widths = table::distribute_column_widths(budget, &rules);
    let floors: Vec<u16> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| table::floor(c, i == 0, Some(i) == sorted))
        .collect();
    table::fit(&mut widths, &floors, budget);
    // Two for the border, one for the header row: the same arithmetic `height` above used.
    let inner_x = area.x + 1;
    let mut x = inner_x + u16::try_from(table::GUTTER.width()).unwrap_or(0);
    let mut spans = Vec::with_capacity(widths.len());
    for (i, w) in widths.iter().enumerate() {
        spans.push((x, *w, shown[i]));
        x = x.saturating_add(*w).saturating_add(table::SPACING);
    }
    app.hits.borrow_mut().tables.push(crate::mouse::TableHit {
        pane: spec.pane,
        block: area,
        header: Rect {
            x: inner_x,
            y: area.y + 1,
            width: inner,
            height: 1,
        },
        rows: Rect {
            x: inner_x,
            y: area.y + 2,
            width: inner,
            height: area.height.saturating_sub(3),
        },
        offset,
        len: rows.len(),
        columns: spans,
    });
    let constraints: Vec<Constraint> = widths.iter().copied().map(Constraint::Length).collect();

    let status = table::status_index(&cols);
    let header = Row::new(cols.iter().enumerate().map(|(i, c)| {
        let Some((_, reverse)) = spec.sort.filter(|_| Some(i) == sorted) else {
            return Cell::from(Span::styled(c.header, theme::header_row()));
        };
        // Cut to leave the mark its cells: the arrow is the last cell of the heading whatever
        // width the column was given, so a heading longer than its cap still shows it.
        let room = usize::from(widths[i].saturating_sub(table::SORT_MARK_CELLS));
        Cell::from(Line::from(vec![
            Span::styled(cut(c.header, room), theme::header_row()),
            Span::styled(
                table::sort_mark(reverse),
                Style::default()
                    .fg(theme::sorter())
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
    }));
    let body = cells.iter().map(|row| {
        let role = status
            .map(|i| status::role_in(spec.key.kind, &row[i].text))
            .unwrap_or(nutsh_catalog::Role::Neutral);
        Row::new(row.iter().enumerate().map(|(i, r)| {
            let style = if cols[i].path == table::CONTEXT_COLUMN.path {
                Style::default()
                    .fg(context_colour(live, &r.text))
                    .add_modifier(Modifier::BOLD)
            } else if Some(i) == status {
                Style::default()
                    .fg(theme::role_fg(role))
                    .add_modifier(Modifier::BOLD)
            } else if r.dim {
                // One branch rather than a `ColumnKind::Timestamp` test: an age is dim
                // because `render_cell` says so, and so are an empty cell and an unresolved
                // reference.
                theme::dim()
            } else {
                Style::default().fg(theme::row_fg(role))
            };
            Cell::from(Span::styled(r.text.as_str(), style))
        }))
    });

    let mut title = spec.title;
    if col_offset > 0 {
        title.push(Span::styled(format!(" ‹{col_offset}"), theme::dim()));
    }
    // What `visible` started from, less what it kept.
    let dropped = all.len() - col_offset - cols.len();
    if dropped > 0 {
        title.push(Span::styled(format!(" »{dropped}"), theme::dim()));
    }

    let widget = Table::new(body, constraints)
        .header(header)
        .column_spacing(table::SPACING)
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(table::GUTTER)
        .highlight_spacing(HighlightSpacing::Always)
        .block(table_block(title, spec.focused));
    let mut state = TableState::default()
        .with_selected(spec.cursor.then(|| spec.selected.saturating_sub(offset)))
        .with_offset(0);
    f.render_stateful_widget(widget, area, &mut state);
    // `height`, not the area's own: it is the row arithmetic this function already did, and a
    // frame with room for the border and the header row and nothing else has nowhere to put a
    // note.
    if let Some(note) = note
        && height > 0
    {
        f.render_widget(
            Paragraph::new(note_line(&note)),
            Rect {
                x: area.x + 1,
                y: area.y + 2,
                width: inner,
                height: 1,
            },
        );
    }
}

/// One colour per joined Prism Central, the session's first, so a merged table reads by colour
/// before it reads by name. The header's `+peer` is painted with the same one.
pub(super) fn context_colour(live: &Live, name: &str) -> ratatui::style::Color {
    let at = if name == live.primary_name() {
        0
    } else {
        live.peers
            .iter()
            .position(|p| &*p.name == name)
            .map_or(0, |i| i + 1)
    };
    let palette = [
        theme::mauve(),
        theme::teal(),
        theme::peach(),
        theme::yellow(),
        theme::sapphire(),
        theme::green(),
    ];
    palette[at % palette.len()]
}
