//! The `:` palette.

use super::*;

/// The suggestions under the `:` prompt: as many ranked entries as fit around the selection,
/// each tagged with what it is - `cmd`, the kind's id, or the reason it is greyed.
pub(super) fn draw_palette(app: &App, f: &mut Frame, body: Rect) {
    let Some(p) = &app.palette else {
        return;
    };
    // One row even with nothing to list, so the box says why it is empty rather than vanish.
    let area = popup_area(body, p.entries.len().max(1));
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
    let mut items: Vec<ListItem> = window
        .iter()
        .map(|entry| match entry {
            Entry::Command(c) => tagged_row(
                &format!(":{}", c.label()),
                Style::default().fg(theme::peach()),
                Span::styled("cmd", theme::dim()),
                width,
            ),
            Entry::Value { text, tag } => tagged_row(
                text,
                Style::default().fg(theme::text()),
                match tag {
                    Tag::Context => Span::styled("ctx", Style::default().fg(theme::mauve())),
                    Tag::Skin => Span::styled("skin", Style::default().fg(theme::sapphire())),
                    // One tag for both verbs: the row is a menu name either way, and the
                    // popup's own title says which of the two is being completed.
                    Tag::Nav { .. } => Span::styled("menu", Style::default().fg(theme::green())),
                    Tag::Interval => Span::styled("every", Style::default().fg(theme::peach())),
                    Tag::Level => Span::styled("level", Style::default().fg(theme::sapphire())),
                    Tag::Format => Span::styled("format", Style::default().fg(theme::peach())),
                },
                width,
            ),
            Entry::Page(def) => tagged_row(
                def.title,
                Style::default().fg(theme::text()),
                Span::styled("page", Style::default().fg(theme::teal())),
                width,
            ),
            Entry::Kind { kind, reason } => greyable_row(
                kind.display,
                Span::styled(elide_left(kind.id, 16), theme::dim()),
                reason.as_deref(),
                width,
            ),
            // The same shape `open from Virtual Machines` already uses: `greyable_row` puts the
            // reason where the tag would be, so the row reads like every other greyed one. The
            // tag is what a `Missing` item without a note would show, and all three have one.
            Entry::Missing(item) => greyable_row(
                item.label,
                Span::styled("nav", theme::dim()),
                item.note,
                width,
            ),
        })
        .collect();
    if items.is_empty() {
        items.push(ListItem::new(Span::styled("no matches", theme::dim())));
    }
    // The placeholder is not a row `enter` could take, so nothing is selected on it.
    let selected = (!p.entries.is_empty()).then_some(p.selected - offset);
    let mut state = ListState::default().with_selected(selected);
    // Titled for what is being completed rather than for what the list happens to hold: the noun
    // is the slot's, and the head - commands, kinds, nav labels and pages in one ranking - is
    // called `kinds & commands`. A slot that completed nothing collapsed to the command row and
    // is `None`, which reads as the head, which is what that row is.
    let noun = p.slot.map_or("kinds & commands", Slot::noun);
    render_framed_list(
        f,
        area,
        items,
        Line::from(Span::styled(
            // Forty cells inside a 44-cell interior at the widest noun, so the box does not
            // grow.
            format!(" {noun} (⇥ complete · ↑↓ · ⏎) "),
            theme::title(),
        )),
        Scroll::of(offset, window.len(), p.entries.len()),
        &mut state,
    );
}
