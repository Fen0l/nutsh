//! The settings screen.

use super::*;

/// Three columns: what it is, what it is now, and where that came from. The chrome is
/// `draw_skins`', because a second popper-upper is a second thing to keep looking the same.
pub(super) fn draw_settings(app: &App, f: &mut Frame, body: Rect) {
    let Some(s) = &app.settings else {
        return;
    };
    let area = centered_rect_with_min(70, 70, 64, 16, body);
    clear_region(f, area);
    let rows = usize::from(area.height.saturating_sub(2));
    // Two cells for the border, then two more for the `▌ ` gutter `HighlightSpacing::Always`
    // reserves on every row, selected or not - so a heading's rule stops where the box does.
    let inner = usize::from(area.width.saturating_sub(2)).saturating_sub(2);
    let (offset, window) = s.window(rows);
    let source_width = 13;
    // Twenty-six cells, which is what `refresh · Virtual Machines` measures: the longest label
    // the screen draws is a kind's display name behind a verb, and a label column that elided
    // the kind would leave two rows both reading `refresh · …`.
    let label_width = 26;
    let value_width = inner.saturating_sub(label_width + source_width);
    let items: Vec<ListItem> = window
        .iter()
        .map(|row| match row {
            crate::settings::Row::Heading(text) => ListItem::new(Line::from(Span::styled(
                format!(
                    "{text} {}",
                    "─".repeat(inner.saturating_sub(text.width() + 1))
                ),
                theme::dim(),
            ))),
            crate::settings::Row::Run { label } => ListItem::new(Line::from(Span::styled(
                format!("⏎ {label}"),
                Style::default().fg(theme::text()),
            ))),
            crate::settings::Row::Note(text) => {
                ListItem::new(Line::from(Span::styled(*text, theme::dim())))
            }
            crate::settings::Row::Namespace {
                name,
                value,
                source,
                open,
                custom,
            } => {
                let mark = if *open { '▾' } else { '▸' };
                let label = if *custom > 0 {
                    format!("{mark} {name} ({custom} set)")
                } else {
                    format!("{mark} {name}")
                };
                let age_width = 8;
                let set_width = value_width.saturating_sub(age_width);
                ListItem::new(Line::from(vec![
                    Span::styled(
                        pad(cut(&label, label_width), label_width),
                        Style::default().fg(theme::text()),
                    ),
                    Span::styled(
                        pad(cut(value, set_width), set_width),
                        Style::default().fg(theme::text()),
                    ),
                    Span::styled(pad(String::new(), age_width), theme::dim()),
                    Span::styled(cut(source, source_width), theme::dim()),
                ]))
            }
            crate::settings::Row::Hidden { name, source } => ListItem::new(Line::from(vec![
                Span::styled(
                    pad(
                        cut(name, label_width + value_width),
                        label_width + value_width,
                    ),
                    Style::default().fg(theme::text()),
                ),
                Span::styled(cut(source, source_width), theme::dim()),
            ])),
            crate::settings::Row::Setting {
                label,
                value,
                source,
                fixed,
                age,
                ..
            } => {
                // A row the screen can only show is dim end to end: the value is real, the
                // keystroke is not.
                let style = if fixed.is_some() {
                    theme::dim()
                } else {
                    Style::default().fg(theme::text())
                };
                // The age shares the value column rather than taking one of its own: a
                // fourth column would cost the source column the width it needs at 80.
                let age_width = 8;
                let set_width = value_width.saturating_sub(age_width);
                ListItem::new(Line::from(vec![
                    Span::styled(pad(cut(label, label_width), label_width), theme::dim()),
                    Span::styled(pad(cut(value, set_width), set_width), style),
                    Span::styled(
                        pad(cut(age.as_deref().unwrap_or("-"), age_width), age_width),
                        theme::dim(),
                    ),
                    Span::styled(cut(source, source_width), theme::dim()),
                ]))
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(s.selected.saturating_sub(offset)));
    render_framed_list(
        f,
        area,
        items,
        Line::from(Span::styled(" settings ", theme::title())),
        Scroll::of(offset, window.len(), s.rows.len()),
        &mut state,
    );
}
