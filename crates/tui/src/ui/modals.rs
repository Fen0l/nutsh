//! The action menu, the picker, the confirm box and the skin list.

use super::*;

/// The narrowest a confirm dialog is drawn: `Delete 7 Virtual Machines. Type DELETE 7 to
/// confirm:` is fifty-one cells, and a prompt cut in half is one nobody can answer.
pub(super) const CONFIRM_WIDTH: u16 = 54;

/// The child kinds of the row `enter` was pressed on, greyed where the row cannot open them.
pub(super) fn draw_picker(app: &App, f: &mut Frame, body: Rect) {
    let Some(p) = &app.picker else {
        return;
    };
    let area = centered_rect_with_min(60, 50, 40, 10, body);
    clear_region(f, area);
    let rows = usize::from(area.height.saturating_sub(2));
    let (offset, window) = p.window(rows);
    // A modal owns the pointer while it is up: what is outside its list is inert, and the
    // three list modals are the three that take one at all.
    app.hits.borrow_mut().modal = Some(crate::mouse::ListHit {
        rows: Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        },
        offset,
        len: p.entries.len(),
    });
    let width = usize::from(area.width);
    let items: Vec<ListItem> = window
        .iter()
        .map(|e| match e {
            // The rule that names the group below it. Two cells narrower than the box, for the
            // border, and two more for the `▌ ` gutter every row of this list reserves.
            crate::picker::Entry::Caption(group) => ListItem::new(Line::from(Span::styled(
                rule(group.caption(), width.saturating_sub(4)),
                theme::dim(),
            ))),
            crate::picker::Entry::Child { kind, .. } => greyable_row(
                kind.display,
                Span::styled(elide_left(kind.id, 16), theme::dim()),
                e.reason(),
                width,
            ),
            // The key the overlay curated, exactly as the `a` menu tags it: the box is where a
            // reader who has never pressed `a` finds out that `p` is power on.
            crate::picker::Entry::Act { action, .. } => greyable_row(
                action.title(),
                Span::styled(key_hint(action), theme::dim()),
                e.reason(),
                width,
            ),
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(p.selected - offset));
    // The captions are chrome, as the menu's rule is: the counts are of rows `enter` can take.
    let takeable = |rows: &[crate::picker::Entry]| {
        rows.iter()
            .filter(|e| !matches!(e, crate::picker::Entry::Caption(_)))
            .count()
    };
    render_framed_list(
        f,
        area,
        items,
        Line::from(vec![
            // Not `open under`: the box lists what can be done to the row as well as what can
            // be opened under it, and the captions inside say which is which.
            Span::styled(" on ", theme::title()),
            Span::styled(
                format!("{} {} ", p.parent.display, cut(&p.parent_name, 24)),
                Style::default().fg(theme::peach()),
            ),
        ]),
        Scroll::of(
            takeable(&p.entries[..offset]),
            takeable(window),
            takeable(&p.entries),
        ),
        &mut state,
    );
}

/// The actions of the row kind's `action_target`, greyed where this session, this account or a
/// local rule refuses them. Three columns, like the palette and the picker: the title, and
/// either the key that runs it or the reason it cannot run.
pub(super) fn draw_menu(app: &App, f: &mut Frame, body: Rect) {
    let Some(m) = &app.menu else {
        return;
    };
    // The rule between the curated workflows and the generated names is a row of the list, so
    // it is measured, scrolled and windowed with them rather than beside them.
    let (shown, cursor) = m.shown();
    // One row even with nothing to list, so the box says why it is empty rather than vanish.
    let area = dialog_area(body, shown.len().max(1), DIALOG_PERCENT, POPUP_WIDTH);
    clear_region(f, area);
    let rows = usize::from(area.height.saturating_sub(2));
    let (offset, window) = crate::palette::window(&shown, cursor.unwrap_or(0), rows);
    let width = usize::from(area.width);
    let mut items: Vec<ListItem> = window
        .iter()
        .map(|row| match row {
            // Two cells for the border and two for the always-reserved gutter, as the picker's
            // captions measure themselves.
            crate::menu::Shown::Boundary => ListItem::new(Line::from(Span::styled(
                rule(crate::menu::RAW_CAPTION, width.saturating_sub(4)),
                theme::dim(),
            ))),
            // The key the overlay curated, or the reason the row cannot be taken.
            crate::menu::Shown::Row(row) => greyable_row(
                row.action.title(),
                Span::styled(key_hint(row.action), theme::dim()),
                row.reason.as_deref(),
                width,
            ),
        })
        .collect();
    if items.is_empty() {
        items.push(ListItem::new(Span::styled("no matches", theme::dim())));
    }
    // The placeholder is not a row `enter` could take, so nothing is selected on it.
    let mut state = ListState::default().with_selected(cursor.map(|c| c - offset));
    // Actions, not rows: the rule is one of the rows and none of the actions, and `↓6` on the
    // border is a promise about things the user can do.
    let acts = |rows: &[crate::menu::Shown]| {
        rows.iter()
            .filter(|row| matches!(row, crate::menu::Shown::Row(_)))
            .count()
    };
    render_framed_list(
        f,
        area,
        items,
        Line::from(vec![
            Span::styled(" act on ", theme::title()),
            Span::styled(
                format!("{} ", cut(&m.subject, 28)),
                Style::default().fg(theme::peach()),
            ),
        ]),
        Scroll::of(acts(&shown[..offset]), acts(window), acts(&shown)),
        &mut state,
    );
}

/// The prompt, the rows a bulk action would touch, and - for a type-name confirm - what has
/// been typed towards the phrase it wants.
pub(super) fn draw_confirm(app: &App, f: &mut Frame, body: Rect) {
    let Some(c) = &app.confirm else {
        return;
    };
    let rows = 1 + c.names.len() + usize::from(c.more > 0) + usize::from(c.expect.is_some());
    let area = dialog_area(body, rows, DIALOG_PERCENT, CONFIRM_WIDTH);
    clear_region(f, area);
    // Two for the border and two for the padding the block adds.
    let room = usize::from(area.width.saturating_sub(4));
    let mut lines = vec![Line::from(Span::styled(
        cut(&c.prompt, room),
        Style::default().fg(theme::text()),
    ))];
    lines.extend(c.names.iter().map(|name| {
        Line::from(Span::styled(
            format!("  {}", cut(name, room.saturating_sub(2))),
            theme::dim(),
        ))
    }));
    if c.more > 0 {
        lines.push(Line::from(Span::styled(
            format!("  … and {} more", c.more),
            theme::dim(),
        )));
    }
    if c.expect.is_some() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                cut(&c.input, room.saturating_sub(3)),
                Style::default().fg(theme::text()),
            ),
            Span::styled("█", Style::default().fg(theme::mauve())),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme::border_focused())
                .padding(Padding::horizontal(1))
                .title(Line::from(Span::styled(" confirm ", theme::title()))),
        ),
        area,
    );
}

/// The built-in skins, with the one showing marked.
pub(super) fn draw_skins(app: &App, f: &mut Frame, body: Rect) {
    let Some(s) = &app.skins else {
        return;
    };
    let area = centered_rect_with_min(40, 50, 32, 10, body);
    clear_region(f, area);
    let rows = usize::from(area.height.saturating_sub(2));
    let (offset, window) = s.window(rows);
    // A modal owns the pointer while it is up: what is outside its list is inert, and the
    // three list modals are the three that take one at all.
    app.hits.borrow_mut().modal = Some(crate::mouse::ListHit {
        rows: Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        },
        offset,
        len: crate::theme::BUILTIN_NAMES.len(),
    });
    let current = theme::current_name();
    let items: Vec<ListItem> = window
        .iter()
        .map(|name| {
            ListItem::new(Line::from(vec![
                Span::styled(*name, Style::default().fg(theme::text())),
                Span::styled(
                    if **name == current { " •" } else { "" },
                    Style::default().fg(theme::teal()),
                ),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(s.selected - offset));
    render_framed_list(
        f,
        area,
        items,
        Line::from(Span::styled(" skin ", theme::title())),
        Scroll::of(offset, window.len(), crate::theme::BUILTIN_NAMES.len()),
        &mut state,
    );
}
