//! The sidebar tree.

use super::*;

pub(super) fn draw_sidebar(app: &App, f: &mut Frame, area: Rect) {
    let focused = app.focus == crate::app::Focus::Sidebar;
    let rows = usize::from(area.height.saturating_sub(2));
    // The window is derived, never stored: `palette::centred` frames the selection against the
    // pane this frame actually has - the same shape as `palette::window`, which the palette,
    // the picker and the detail pane use - so the `▌` bar, the slice and the `↑n/↓n` counts are
    // one number and the highlight cannot name a row the frame is not showing.
    //
    // `centred` rather than `window`, and only here: the menu is a list you live in.
    let s = &app.sidebar;
    let all = s.rows();
    let (offset, window) = crate::palette::centred(all, s.selected, rows);
    // Two for the border. The list's own gutter is inside `rows`, and a click anywhere across
    // the row selects it, so the gutter needs no span of its own.
    app.hits.borrow_mut().sidebar = Some(crate::mouse::ListHit {
        rows: Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        },
        offset,
        len: all.len(),
    });
    // The label's budget, per row kind: two cells of highlight gutter, then `▸ ` on a group
    // and four cells of indent and marker on an item.
    let title_width = usize::from(area.width.saturating_sub(4));
    let group_width = usize::from(area.width).saturating_sub(6);
    let item_width = group_width.saturating_sub(2);
    // The root of the stack, not its top: the marked item names the table that was opened, and
    // a drill-down into a row of it is not what `[n]` counts.
    // A page counts nothing: its rows belong to its panes, each of which carries its own.
    let count = app.live.as_ref().map_or(0, |l| match l.stack.first() {
        Some(View::Table(v)) => l.merged(&v.key).rows.len(),
        Some(View::Page(_)) | None => 0,
    });
    // A kind whose table came back 404 is greyed like a `Missing` item, but only once the
    // answer is in: the store holds a table for a key only after a cycle has reported on it,
    // so an unopened kind is not greyed and cannot be.
    //
    // Asked of the store rather than of the catalog, and the direction matters on every frame:
    // the store holds the handful of tables this session has opened, while `catalog::kind` is
    // a linear scan of every kind there is, and this runs once per drawn menu row. The key
    // shape is `TableKey::top`'s - no parents, no filter - spelled out because it is being
    // matched rather than built.
    let not_served = |target: nutsh_catalog::NavTarget| match (target, app.live.as_ref()) {
        (nutsh_catalog::NavTarget::Kind(id), Some(live)) => live.store.tables().any(|(k, t)| {
            t.not_served && k.kind.id == id && k.parents.is_empty() && k.filter.is_none()
        }),
        _ => false,
    };

    let items: Vec<ListItem> = window
        .iter()
        .map(|row| match row {
            // What follows is not more menu: it is the rest of the catalog, filed under the
            // namespace it came from, for the 119 kinds nobody has curated a place for.
            crate::sidebar::Row::Separator => ListItem::new(Line::from(Span::styled(
                rule(" by namespace ", group_width + 2),
                theme::dim(),
            ))),
            crate::sidebar::Row::Group { group } => {
                // Nothing is ever silently gone, which is the whole risk of this feature - so
                // the count is taken out of the name's budget rather than pushed off the pane,
                // the way an open item's `[n]` tally is below.
                let dropped = s.dropped(*group);
                let count = (dropped > 0).then(|| format!(" ({dropped} hidden)"));
                let name_width = group_width.saturating_sub(count.as_deref().map_or(0, str::width));
                let mut spans = vec![Span::styled(
                    format!(
                        "{} {}",
                        if s.is_collapsed(*group) && s.filter().is_empty() {
                            "▸"
                        } else {
                            "▾"
                        },
                        cut(NAV[*group].name, name_width)
                    ),
                    theme::dim().add_modifier(Modifier::BOLD),
                )];
                if let Some(count) = count {
                    spans.push(Span::styled(count, theme::dim()));
                }
                ListItem::new(Line::from(spans))
            }
            crate::sidebar::Row::Item { group, item } => {
                let it = &NAV[*group].items[*item];
                let open = s.open == Some(it.target);
                let tally = (open && count > 0).then(|| format!(" [{count}]"));
                // The count is the point of marking the row: the label gives way to it rather
                // than pushing it off the pane.
                let label_width = item_width.saturating_sub(tally.as_deref().map_or(0, str::width));
                let mut spans = vec![Span::raw(if open { "  • " } else { "    " })];
                match s.reason(*group, *item) {
                    // Not in the v4 API, or a namespace this Prism Central answered at no
                    // version: the same drawing, because to a reader they are the same fact -
                    // "you cannot open this, and here is why". `enter` says the reason in full
                    // whatever the pane's width, and it is the same sentence the palette tags
                    // its own greyed row with and `open_root` refuses with.
                    Some(reason) if !open => {
                        let label = cut(it.label, label_width);
                        let left = label_width.saturating_sub(label.width());
                        spans.push(Span::styled(label, Style::default().fg(theme::overlay1())));
                        // Only when it fits whole. A 24-cell pane has none of the room the
                        // reason needs, and a torn `not in the v4 API` reads as a bug rather
                        // than an explanation; §11.1's frame draws the item greyed with
                        // nothing beside it, and `enter` is what says why.
                        if reason.width() < left {
                            spans.push(Span::styled(format!(" {reason}"), theme::dim()));
                        }
                    }
                    // Ahead of `open`, which is the item the 404 came back for: the answer is
                    // about the kind and stands whether or not the cursor is on it. The `• `
                    // marker above is what says which item is open, and it is drawn either
                    // way, so greying the label costs the frame nothing it needs.
                    //
                    // The order matters and is not the obvious one. Writing `if open { accent }
                    // else if not_served { … }` reads better and is wrong: an open item whose
                    // table 404s must still grey, and the only table that can be `not_served`
                    // is one that was opened, so `not_served` has to be asked first.
                    _ => spans.push(Span::styled(
                        cut(it.label, label_width),
                        if not_served(it.target) {
                            Style::default().fg(theme::overlay1())
                        } else if open {
                            theme::accent()
                        } else if s.is_hidden(*group, *item) {
                            // Hidden, and on screen only because the filter matched it.
                            theme::dim()
                        } else {
                            Style::default().fg(theme::text())
                        },
                    )),
                }
                if let Some(tally) = tally {
                    spans.push(Span::styled(tally, Style::default().fg(theme::counter())));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();

    // The context name is the more useful of the two once there is more than one Prism Central.
    // `:all` suspends rule 2's dropping for the session, and a menu that is showing more than
    // it was asked to has to say so where the menu itself is.
    let name = app
        .live
        .as_ref()
        .and_then(|l| l.session.context.clone())
        .unwrap_or_else(|| "nutsh".to_string());
    // And it may not say `(all)` while an explicit hide keeps a group out: rule 1 is not lifted
    // by `:all`, so the marker counts what is still hidden rather than claiming there is none.
    let title = match (app.nav_all, app.nav_hide.len()) {
        (false, _) => name,
        (true, 0) => format!("{name} (all)"),
        (true, n) => format!("{name} (all but {n})"),
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            theme::border_focused()
        } else {
            theme::border()
        })
        .title(Span::styled(
            format!(" {} ", cut(&title, title_width)),
            theme::title(),
        ));
    // The counts of what is hidden go on the border, which costs the list no row - the same
    // idea as the modal's `(n more)`.
    let above = offset;
    let below = all.len().saturating_sub(offset + window.len());
    if above > 0 {
        block = block.title(
            Line::from(Span::styled(format!(" ↑{above} "), theme::dim()))
                .alignment(Alignment::Right),
        );
    }
    if below > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(format!(" ↓{below} "), theme::dim()))
                .alignment(Alignment::Right),
        );
    }
    let list = List::new(items)
        .highlight_style(theme::selected_row())
        .highlight_symbol("▌ ")
        .highlight_spacing(HighlightSpacing::Always)
        .block(block);
    // The bar only where the keys are: when focus is in the body the selection is drawn in
    // `accent` without it, so it is clear which pane the arrow keys will move.
    let mut state = ListState::default().with_selected(if focused {
        Some(s.selected.saturating_sub(offset))
    } else {
        None
    });
    f.render_stateful_widget(list, area, &mut state);
}
