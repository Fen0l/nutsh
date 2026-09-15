//! `:activity`: the requests, and the tables.

use super::*;

/// `PATH` is the one column that grows; every other has a width its content fits in.
const REQUEST_WIDTHS: [Constraint; 6] = [
    Constraint::Length(5), // WHEN
    Constraint::Length(6), // METHOD
    Constraint::Length(6), // STATUS
    Constraint::Length(6), // MS
    Constraint::Length(9), // VIA
    Constraint::Min(20),   // PATH
];

const TABLE_WIDTHS: [Constraint; 7] = [
    Constraint::Length(5),  // WHEN
    Constraint::Length(22), // KIND
    Constraint::Length(6),  // ROWS
    Constraint::Length(6),  // TOTAL
    Constraint::Length(5),  // FROM
    Constraint::Length(12), // STATE
    Constraint::Min(10),    // SCOPE
];

pub(super) fn draw_activity(app: &App, f: &mut Frame, body: Rect) {
    let (Some(live), Some(view)) = (&app.live, &app.activity_view) else {
        return;
    };
    clear_region(f, body);
    let rows_shown = usize::from(body.height.saturating_sub(3));
    let offset = (view.selected + 1).saturating_sub(rows_shown);
    let mut state = TableState::default().with_selected(Some(view.selected.saturating_sub(offset)));
    match view.tab {
        crate::activity::Tab::Requests => {
            let calls = live.session.client.metrics().calls();
            let now = std::time::Instant::now();
            let title = vec![
                Span::styled(" activity · requests", theme::title()),
                Span::styled(format!(" [{}]", calls.len()), theme::dim()),
                Span::styled(" ⇥ tables", theme::dim()),
            ];
            if calls.is_empty() {
                f.render_widget(
                    Paragraph::new(Span::styled(" No request yet.", theme::dim()))
                        .block(table_block(title, true)),
                    body,
                );
                return;
            }
            let rows = calls.iter().skip(offset).take(rows_shown).map(|c| {
                let status = match c.status {
                    Some(s) => s.to_string(),
                    None => "-".to_string(),
                };
                let status_style = match c.status {
                    Some(401 | 429) | None => Style::default().fg(theme::red()),
                    Some(500..) => Style::default().fg(theme::red()),
                    Some(304) => theme::dim(),
                    Some(400..) => Style::default().fg(theme::yellow()),
                    _ => Style::default().fg(theme::text()),
                };
                let via = if c.paced {
                    format!("{}·paced", c.credential)
                } else {
                    c.credential.to_string()
                };
                Row::new(vec![
                    Cell::from(Span::styled(
                        nutsh_core::cell::span(now.saturating_duration_since(c.at).as_secs()),
                        theme::dim(),
                    )),
                    Cell::from(c.method.as_str()),
                    Cell::from(Span::styled(status, status_style)),
                    Cell::from(Span::styled(c.ms.to_string(), theme::dim())),
                    Cell::from(Span::styled(via, theme::dim())),
                    Cell::from(c.path.as_str()),
                ])
            });
            let header = Row::new(
                ["WHEN", "METHOD", "STATUS", "MS", "VIA", "PATH"]
                    .map(|h| Cell::from(Span::styled(h, theme::header_row()))),
            );
            f.render_stateful_widget(
                Table::new(rows, REQUEST_WIDTHS)
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
        crate::activity::Tab::Tables => {
            let lines = app.activity_tables();
            let title = vec![
                Span::styled(" activity · tables", theme::title()),
                Span::styled(format!(" [{}]", lines.len()), theme::dim()),
                Span::styled(" ⇥ requests", theme::dim()),
            ];
            if lines.is_empty() {
                f.render_widget(
                    Paragraph::new(Span::styled(" No table held yet.", theme::dim()))
                        .block(table_block(title, true)),
                    body,
                );
                return;
            }
            let rows = lines.iter().skip(offset).take(rows_shown).map(|l| {
                Row::new(vec![
                    Cell::from(Span::styled(
                        l.age.map_or("never".to_string(), nutsh_core::cell::span),
                        theme::dim(),
                    )),
                    Cell::from(l.kind.display),
                    Cell::from(l.rows.to_string()),
                    Cell::from(Span::styled(
                        l.total.map_or("-".to_string(), |t| t.to_string()),
                        theme::dim(),
                    )),
                    Cell::from(Span::styled(l.from, theme::dim())),
                    Cell::from(Span::styled(
                        l.state.as_str(),
                        if l.state == "ok" {
                            theme::dim()
                        } else {
                            Style::default().fg(theme::yellow())
                        },
                    )),
                    Cell::from(Span::styled(l.scope.as_str(), theme::dim())),
                ])
            });
            let header = Row::new(
                ["WHEN", "KIND", "ROWS", "TOTAL", "FROM", "STATE", "SCOPE"]
                    .map(|h| Cell::from(Span::styled(h, theme::header_row()))),
            );
            f.render_stateful_widget(
                Table::new(rows, TABLE_WIDTHS)
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
    }
}
