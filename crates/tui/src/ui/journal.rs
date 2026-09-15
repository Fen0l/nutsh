//! The journal.

use super::*;

/// The journal's columns, summing to the 82 cells a 120-column frame leaves beside the menu.
///
/// `TASK` is the only one that grows: a Prism task extId is a base64 prefix and a UUID, it is
/// the pointer out of this view into the API and the audit trail, and every column before it
/// has a width its content fits in. `NAME` holds an ordinary VM name (`web-01`, and the
/// eighteen-cell ones a naming convention produces), `ACTION` holds `guest-shutdown` and
/// `power-cycle`, and `OUTCOME` holds `read-only session` - the longest of the refusals the
/// menu greys a row with - before it starts cutting a failure message. So `TASK` is also the
/// column a wider frame and a hidden menu (`^b`) pay out to: a full extId is 45 cells, which no
/// 120-column frame can show beside six other columns, and 30 of them beside none.
pub(super) const JOURNAL_WIDTHS: [Constraint; 7] = [
    Constraint::Length(5),  // WHEN
    Constraint::Length(10), // CONTEXT
    Constraint::Length(10), // KIND
    Constraint::Length(18), // NAME
    Constraint::Length(14), // ACTION
    Constraint::Length(17), // OUTCOME
    Constraint::Min(8),     // TASK
];

/// `:journal`, over the whole body: what was attempted this session, newest first.
///
/// `WHEN` is an age rendered the way a `Timestamp` cell is, so a journal row and a table row
/// never disagree about how long ago something happened.
pub(super) fn draw_journal(app: &App, f: &mut Frame, body: Rect) {
    let (Some(live), Some(view)) = (&app.live, &app.journal_view) else {
        return;
    };
    clear_region(f, body);
    // No trailing space: `table_block` adds the one that closes the title.
    let title = vec![Span::styled(" journal", theme::title())];
    if live.journal.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                " Nothing attempted this session.",
                theme::dim(),
            ))
            .block(table_block(title, true)),
            body,
        );
        return;
    }
    // Two rows for the border, one for the header, as every other table measures it.
    let rows_shown = usize::from(body.height.saturating_sub(3));
    // The window `palette::window` computes, taken off the ring itself: newest first, at most a
    // screenful drawn, and no five-hundred-entry `Vec` built once a frame to slice a dozen rows
    // out of it.
    let offset = (view.selected + 1).saturating_sub(rows_shown);
    let rows = live
        .journal
        .entries()
        .rev()
        .skip(offset)
        .take(rows_shown)
        .map(|e| {
            Row::new(vec![
                Cell::from(Span::styled(
                    nutsh_core::cell::age(e.at, app.now),
                    theme::dim(),
                )),
                Cell::from(e.context.as_str()),
                Cell::from(Span::styled(kind_label(e.kind), theme::dim())),
                Cell::from(e.name.as_str()),
                Cell::from(e.action),
                Cell::from(e.outcome.label()),
                Cell::from(Span::styled(
                    e.task_ext_id.as_deref().unwrap_or("-"),
                    theme::dim(),
                )),
            ])
        });
    let header = Row::new(
        [
            "WHEN", "CONTEXT", "KIND", "NAME", "ACTION", "OUTCOME", "TASK",
        ]
        .map(|h| Cell::from(Span::styled(h, theme::header_row()))),
    );
    let mut state = TableState::default().with_selected(Some(view.selected.saturating_sub(offset)));
    f.render_stateful_widget(
        Table::new(rows, JOURNAL_WIDTHS)
            .header(header)
            .column_spacing(table::SPACING)
            .row_highlight_style(theme::selected_row())
            .highlight_symbol(table::GUTTER)
            .highlight_spacing(HighlightSpacing::Always)
            .block(table_block(title, true)),
        body,
        &mut state,
    );
}

/// A journal row names the kind by the word the palette and `:can-i` take - `vm`, `task` -
/// rather than by its display name: it is the shorter of the two, this column is ten cells
/// wide, and it is the spelling a reader would type to go and look. Read off the `Kind` the
/// entry carries, so no row costs a catalog lookup.
pub(super) fn kind_label(kind: &'static nutsh_catalog::Kind) -> &'static str {
    kind.aliases.first().copied().unwrap_or(kind.id)
}
