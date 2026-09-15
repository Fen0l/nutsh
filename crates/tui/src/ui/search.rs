//! The search results box.

use super::*;

/// The rows the reach line and the box's two borders take off the results list.
pub(super) const SEARCH_CHROME: u16 = 3;

/// `:search`, over the whole body: what every loaded kind holds under one term, grouped by
/// kind, with the reach under it.
///
/// The reach line is drawn last and is never scrolled away. It is the sentence that makes the
/// list readable: a search of two kinds out of two hundred and sixty-two looks exactly like a
/// search of all of them until something says which it was, and a caption that scrolled off
/// with the results would say it only to whoever started at the top.
pub(super) fn draw_search(app: &App, f: &mut Frame, body: Rect) {
    let Some(view) = &app.search else {
        return;
    };
    clear_region(f, body);
    // No trailing space: `table_block` adds the one that closes the title.
    let title = vec![
        Span::styled(" results for ", theme::title()),
        Span::styled(
            format!("\"{}\"", view.term),
            Style::default().fg(theme::mauve()),
        ),
        Span::styled(
            format!("  [{}]", view.hits()),
            Style::default().fg(theme::counter()),
        ),
    ];
    let block = table_block(title, true);
    let inner = block.inner(body);
    f.render_widget(block, body);
    let [list, reach] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let rows = usize::from(body.height.saturating_sub(SEARCH_CHROME));
    let (offset, window) = crate::palette::window(&view.rows, view.selected, rows);
    let items: Vec<ListItem> = if view.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "nothing matches, in what is loaded",
            theme::dim(),
        )))]
    } else {
        window
            .iter()
            .map(|row| search_row(row, list.width))
            .collect()
    };
    let mut state = ListState::default()
        .with_selected((!view.is_empty()).then(|| view.selected.saturating_sub(offset)));
    f.render_stateful_widget(
        List::new(items)
            .highlight_style(theme::selected_row())
            .highlight_symbol(table::GUTTER)
            .highlight_spacing(HighlightSpacing::Always),
        list,
        &mut state,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(view.reach(), theme::dim()))),
        reach,
    );
}

/// One line of the results: a kind's caption with its count, or a result indented under it with
/// what it matched on at the right edge.
///
/// The caption carries the kind's own count so a group whose results run past the fold still
/// says how many it has - the one number a scrolled list loses otherwise.
pub(super) fn search_row<'a>(row: &'a crate::search::Row, width: u16) -> ListItem<'a> {
    // Two for the always-reserved gutter, which the list spends on every row.
    let inner = usize::from(width).saturating_sub(table::GUTTER.width());
    match row {
        crate::search::Row::Group { kind, hits } => ListItem::new(Line::from(vec![
            Span::styled(kind.display, theme::title()),
            Span::styled(format!(" [{hits}]"), Style::default().fg(theme::counter())),
        ])),
        crate::search::Row::Hit { name, detail, .. } => tagged_row(
            &format!("  {name}"),
            Style::default().fg(theme::text()),
            Span::styled(detail.as_str(), theme::dim()),
            // `tagged_row` budgets for the border and the gutter itself; this list has the
            // gutter and the block's borders, which is the same four cells.
            inner + table::GUTTER.width(),
        ),
    }
}
