//! A feature page and its panes.

use super::*;

/// The summary column's width, and the narrowest a pane beside it is worth drawing.
pub(super) const SUMMARY_WIDTH: u16 = 26;

pub(super) const PANE_MIN: u16 = 50;

pub(super) fn draw_page(app: &App, f: &mut Frame, area: Rect) {
    let (Some(live), Some(page)) = (&app.live, app.page()) else {
        return;
    };
    // Every pane starts this frame with no room, and `draw_pane` gives back the height of the
    // ones there was space for. A pane in a grid row this body is too short for therefore
    // holds no stale height: `Pane::drawn` is what its budget is computed from, and a pane
    // nobody can see must not go on polling at the size it had when it was last visible.
    for pane in &page.panes {
        pane.drawn.set(0);
    }
    // Not one row fits: the body is too short for a block with a header in it, and the header's
    // `Count:` field is the only place left to say so.
    let rows = page_rows(page.def, area.height);
    if rows == 0 {
        return;
    }
    let constraints: Vec<Constraint> = page.def.row_heights[..rows]
        .iter()
        .map(|h| {
            if *h == 0 {
                Constraint::Min(ROW_MIN)
            } else {
                Constraint::Length(row_height(*h))
            }
        })
        .collect();
    let row_areas = Layout::vertical(constraints).split(area);

    // The summary is a column, not a pane: it spans the first `summary_rows` rows.
    let summary_area = page.def.summary.and_then(|_| {
        let span = usize::from(page.def.summary_rows).min(rows);
        let first = row_areas.first()?;
        let last = row_areas.get(span.checked_sub(1)?)?;
        (area.width > SUMMARY_WIDTH + PANE_MIN).then(|| Rect {
            x: area.x + area.width - SUMMARY_WIDTH,
            y: first.y,
            width: SUMMARY_WIDTH,
            height: last.y + last.height - first.y,
        })
    });

    for (r, row_area) in row_areas.iter().enumerate() {
        let body = match summary_area {
            Some(_) if r < usize::from(page.def.summary_rows) => Rect {
                width: row_area.width - SUMMARY_WIDTH,
                ..*row_area
            },
            _ => *row_area,
        };
        let mut here: Vec<usize> = page
            .panes
            .iter()
            .enumerate()
            .filter(|(_, p)| usize::from(p.def.row) == r)
            .map(|(i, _)| i)
            .collect();
        if here.is_empty() {
            continue;
        }
        // The grid decides the order, not the order the panes were declared in: `(row, col)`
        // is what the generator validates for duplicates and bounds, so it is what the frame
        // has to obey.
        here.sort_by_key(|&i| page.panes[i].def.col);
        let widths: Vec<Constraint> = here
            .iter()
            .map(|&i| Constraint::Fill(page.panes[i].def.weight.max(1)))
            .collect();
        for (slot, &i) in Layout::horizontal(widths).split(body).iter().zip(&here) {
            draw_pane(app, live, page, i, f, *slot);
        }
    }

    if let Some(area) = summary_area {
        draw_summary(page, f, area);
    }
}

/// One pane: the table chrome around its rows, or its own reason instead of them.
pub(super) fn draw_pane(
    app: &App,
    live: &Live,
    page: &PageView,
    i: usize,
    f: &mut Frame,
    area: Rect,
) {
    let pane = &page.panes[i];
    // Two for the border, one for the header row: the same arithmetic `render_rows` does. Set
    // before the reason branch below, so a pane drawn without rows still records the room it
    // had - the 404 it says today may be rows tomorrow.
    pane.drawn.set(usize::from(area.height.saturating_sub(3)));
    // The cursor is the page's, the focus ring is the keyboard's: moving focus to the menu
    // must not make the page forget which row it is on, the way a table view does not.
    let current = page.focus == i;
    let focused = app.body_focused() && current;
    let t = live.store.table(&pane.key);
    let mut title = vec![
        Span::styled(format!(" {} ", pane.def.title), theme::title()),
        Span::styled(
            format!("[{}]", count(t, None)),
            Style::default().fg(theme::counter()),
        ),
    ];
    title.extend(walking(t));
    // What the pane says instead of rows, decided where the table view's is decided - and on
    // the budget the pane's own subscription asked for, not the kind's.
    let note = body_note(t, pane.budget(), pane.stopped, pane.def.empty);
    // A pane with nothing to show says what nothing means for it, inside its own block: a 404
    // on a sub-path this Prism Central does not serve is the pane's news, not the app's.
    //
    // A cycle in flight is the exception: that note goes through `render_rows` below, so the
    // column headings the walk is about are on the same frame as the sentence announcing it.
    let settled = note.as_deref().filter(|_| t.cycle_started.is_none());
    // The pane's own reason only while it has nothing to draw. `PageView::regrade` can give a
    // pane a reason *after* it has rows - a list that 404s on its third cycle - and the store's
    // contract for a failed cycle is that the rows stay: taking them away would leave the pane
    // blank with nothing to bring them back, since the subscription has stopped.
    let reason = pane
        .reason
        .as_deref()
        .filter(|_| t.rows.is_empty())
        .or(settled);
    if let Some(reason) = reason {
        f.render_widget(
            Paragraph::new(note_line(reason)).block(table_block(title, focused)),
            area,
        );
        return;
    }
    render_rows(
        app,
        live,
        Rows {
            key: &pane.key,
            pane: Some(i),
            all: pane.columns(),
            col_offset: 0,
            sort: None,
            selected: pane.selected,
            marks: None,
            cursor: current,
            focused,
            note,
            filter: None,
            title,
        },
        f,
        area,
    );
}

/// The right-hand column: `label` dim, `value` in its role's colour, right-aligned.
pub(super) fn draw_summary(page: &PageView, f: &mut Frame, area: Rect) {
    let width = usize::from(area.width.saturating_sub(2));
    let lines: Vec<Line> = page
        .summary
        .iter()
        .map(|l| {
            // Both halves measured the same way. `{:>n}` pads by character count, and the
            // values Tasks 7 and 8 put here carry unit suffixes and thousands separators.
            let pad = width
                .saturating_sub(l.label.width())
                .saturating_sub(l.value.width());
            Line::from(vec![
                Span::styled(l.label.clone(), theme::dim()),
                Span::raw(" ".repeat(pad)),
                Span::styled(l.value.clone(), Style::default().fg(theme::role_fg(l.role))),
            ])
        })
        .collect();
    f.render_widget(
        // The same chrome the panes beside it wear, from the same place: the summary is a
        // column of the page, not a box of its own.
        // The leading space is the caller's and the trailing one is `table_block`'s, exactly
        // as for a pane's `" {title} "` plus its `[count]`.
        Paragraph::new(lines).block(table_block(
            vec![Span::styled(" Summary", theme::title())],
            false,
        )),
        area,
    );
}
