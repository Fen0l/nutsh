//! Drawing primitives no screen owns: cutting and padding text, popup and dialog areas,
//! and the framed scrolling list.

use super::*;

/// The last `room` columns of `path`, marked with a leading `…` when something was dropped.
/// A path is recognised by its end - the file name, and the directory above it - so clipping
/// it from the right, which is what the terminal would do, throws away the half that
/// identifies it.
pub(super) fn elide_left(path: &str, room: usize) -> String {
    if path.width() <= room {
        return path.to_string();
    }
    match room {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            // The marker takes a column of its own, so the tail is measured against what is
            // left. Counted in display cells: a wide character that only half fits would
            // otherwise push the line one column past the edge.
            let mut used = 0;
            let mut kept = 0;
            for c in path.chars().rev() {
                let w = c.width().unwrap_or(0);
                if used + w > room - 1 {
                    break;
                }
                used += w;
                kept += 1;
            }
            std::iter::once('…')
                .chain(path.chars().skip(path.chars().count() - kept))
                .collect()
        }
    }
}

/// The popup's geometry: bottom-left of the body, one cell in, 46 wide, at most 12 rows.
pub(super) const POPUP_WIDTH: u16 = 46;

pub(super) const POPUP_ROWS: u16 = 12;

pub(super) fn popup_area(body: Rect, rows: usize) -> Rect {
    // Inside the frame of whatever the body draws: never on its top border row, always one
    // row above its bottom one, and never past its right one, so the two boxes never share a
    // corner or an edge.
    let height = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(POPUP_ROWS + 2)
        .min(body.height.saturating_sub(2));
    let width = POPUP_WIDTH.min(body.width.saturating_sub(2));
    Rect {
        x: body.x + 1,
        y: body.y + body.height.saturating_sub(height + 1),
        width,
        height,
    }
}

/// How the `inner` cells of a row are shared between its label and its tag, as `(label,
/// tag)` cells. The label keeps what it needs up to half the row, the tag is cut to what is
/// left, and the label takes the rest. A tag is an id or a reason, and a reason that pushes
/// the name of the kind it explains out of the row explains nothing.
pub(super) fn share(label: &str, tag: &str, inner: usize) -> (usize, usize) {
    let label_floor = label.width().min(inner / 2);
    let tag_cells = tag.width().min(inner.saturating_sub(label_floor + 1));
    (inner.saturating_sub(tag_cells + 1), tag_cells)
}

/// `s` in at most `room` cells, cut from the right with a marker where it was cut: the head
/// of a reason is the half that says what is wrong.
pub(super) fn elide_right(s: &str, room: usize) -> String {
    if s.width() <= room {
        return s.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let mut out = cut(s, room - 1);
    out.push('…');
    out
}

/// One row: the label on the left, cut to its column, and a type tag at the right edge, cut
/// to what the label leaves it. The label is copied into its column, so it can be borrowed
/// from anywhere; the tag is borrowed for the row's life unless it has to be cut.
pub(super) fn tagged_row<'a>(
    label: &str,
    style: Style,
    tag: Span<'a>,
    width: usize,
) -> ListItem<'a> {
    // Two for the border, two for the always-reserved gutter.
    let inner = width.saturating_sub(4);
    let (label_cells, tag_cells) = share(label, &tag.content, inner);
    let tag = if tag.content.width() <= tag_cells {
        tag
    } else {
        Span::styled(elide_right(&tag.content, tag_cells), tag.style)
    };
    ListItem::new(Line::from(vec![
        Span::styled(field(label, label_cells), style),
        Span::raw(" "),
        tag,
    ]))
}

/// A row something can refuse: the label with its tag, or the same label greyed with the reason
/// in place of the tag - the half that says what to do about it. The palette, the picker and
/// the action menu all draw their greyed rows through this, which is why they read alike.
pub(super) fn greyable_row<'a>(
    label: &str,
    tag: Span<'a>,
    reason: Option<&'a str>,
    width: usize,
) -> ListItem<'a> {
    match reason {
        None => tagged_row(label, Style::default().fg(theme::text()), tag, width),
        Some(reason) => tagged_row(
            label,
            Style::default().fg(theme::overlay1()),
            Span::styled(reason, Style::default().fg(theme::overlay1())),
            width,
        ),
    }
}

/// A box sized to what it holds, centred in the body: `rows` rows plus its border, never
/// taller than the body, and `percent` of its width but never under `min_width`.
///
/// Not [`popup_area`], which anchors to the bottom-left corner and caps at twelve rows: an
/// action menu is the length the kind's action list is, and it is read against the whole body
/// rather than beside the `:` prompt it does not have.
pub(super) fn dialog_area(body: Rect, rows: usize, percent: u16, min_width: u16) -> Rect {
    let full = centered_rect_with_min(percent, 100, min_width, 3, body);
    let height = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(full.height);
    Rect {
        y: body.y + (body.height - height) / 2,
        height,
        ..full
    }
}

/// The width the action menu and the confirm dialog ask for, as a percentage of the body.
pub(super) const DIALOG_PERCENT: u16 = 70;

/// `[P]` for an action a key runs, nothing for one only the menu reaches.
pub(super) fn key_hint(action: &'static nutsh_catalog::Action) -> String {
    if action.key.is_empty() {
        String::new()
    } else {
        format!("[{}]", action.key)
    }
}

/// `s` cut to `width` display cells; the line is laid out widest field last, so what falls off
/// the end is what matters least.
///
/// Cells, not characters: ratatui lays a line out in the columns the terminal draws, and a CJK
/// name is two of them per character. Counting characters here would let a line of them run
/// twice as far as the box it is drawn in.
/// `caption` centred on a rule, in exactly `width` cells: `─── by namespace ───`. The caption
/// keeps its own spaces, so a caller decides how much air it wants around the words.
///
/// Crate-internal because the detail pane draws the same rule between the curated workflows and
/// the generated names that the menu and the picker draw, and one shape of rule drawn by three
/// surfaces has to come from one function.
pub(crate) fn rule(caption: &str, width: usize) -> String {
    let caption = cut(caption, width);
    let dashes = width.saturating_sub(caption.width());
    let left = dashes / 2;
    format!("{}{caption}{}", "─".repeat(left), "─".repeat(dashes - left))
}

pub(super) fn cut(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

/// `s` in exactly `width` display cells: cut when it is wider, padded when it is narrower.
/// What `{:<width$}` would do, except that a value too long for its column shifts every column
/// after it instead of being cut, and one long name should not bend the whole table.
pub(super) fn field(s: &str, width: usize) -> String {
    pad(cut(s, width), width)
}

/// `s` in at least `width` display cells. For the column before the one that may be lost: the
/// reason a palette entry is greyed has to survive a narrow terminal, and pushing the id along
/// - which `cut` then takes off the end - is how it does.
pub(super) fn pad(mut s: String, width: usize) -> String {
    s.extend(std::iter::repeat_n(' ', width.saturating_sub(s.width())));
    s
}

/// Clear a popup region before drawing on top of it. `Clear` resets the cells to the terminal
/// default; with the skin background enabled that would punch a transparent hole through the
/// fill, so repaint `base` over the cleared cells.
pub(super) fn clear_region(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    if let Some(bg) = theme::background() {
        f.buffer_mut().set_style(area, Style::default().bg(bg));
    }
}

/// The one list widget every modal uses: a rounded focused border, the `▌ ` gutter always
/// reserved, and the selection in `selected_row()`.
pub(super) fn render_framed_list<'a>(
    f: &mut Frame,
    area: Rect,
    items: Vec<ListItem<'a>>,
    title: Line<'a>,
    scroll: Scroll,
    state: &mut ListState,
) {
    let mut title = title;
    // The count goes inside the title and the arrows go beside it: the arrows say there is
    // more, and only the count says how much of the list this is. It is dropped rather than
    // allowed to overwrite the subject, since a box titled `act on web-0` has lost the one
    // thing a title is for.
    if scroll.scrolls() {
        let count = format!("· {}/{} ", scroll.shown(), scroll.total);
        // The up arrow shares the top border with the title, so it is part of the budget.
        let arrow = if scroll.above > 0 {
            format!(" ↑{} ", scroll.above).width()
        } else {
            0
        };
        if title.width() + count.width() + arrow + 2 <= usize::from(area.width) {
            title.push_span(Span::styled(count, theme::dim()));
        }
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_focused())
        .title(title);
    // On the border, where they cost the list no row. The sidebar has drawn its own this way
    // all along; these are the same two lines lifted into the chrome every modal list shares,
    // so the menu, the palette, the picker, the skins and the settings gained them at once.
    if scroll.above > 0 {
        block = block.title(
            Line::from(Span::styled(format!(" ↑{} ", scroll.above), theme::dim()))
                .alignment(Alignment::Right),
        );
    }
    if scroll.below > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(format!(" ↓{} ", scroll.below), theme::dim()))
                .alignment(Alignment::Right),
        );
    }
    let list = List::new(items)
        .highlight_style(theme::selected_row())
        .highlight_symbol("▌ ")
        .highlight_spacing(HighlightSpacing::Always)
        .block(block);
    f.render_stateful_widget(list, area, state);
}

/// How much of a list its window holds: the rows above it, the rows below it, and the length
/// of the whole thing. A list that fits its box is `above == below == 0` and draws no chrome.
///
/// The units need not be list rows. The action menu counts actions, because its `raw API
/// names` rule is a row of the list but not a thing anybody can do, and `↓6` has to promise
/// six more actions rather than six more rows.
#[derive(Clone, Copy, Default)]
pub(super) struct Scroll {
    above: usize,
    below: usize,
    total: usize,
}

impl Scroll {
    /// The window a `palette::window` call returned: `offset` skipped, `shown` drawn, `total`
    /// in the list.
    pub(super) fn of(offset: usize, shown: usize, total: usize) -> Self {
        Self {
            above: offset,
            below: total.saturating_sub(offset + shown),
            total,
        }
    }

    pub(super) fn scrolls(self) -> bool {
        self.above > 0 || self.below > 0
    }

    pub(super) fn shown(self) -> usize {
        self.total.saturating_sub(self.above + self.below)
    }
}

/// A rectangle that is `percent` of `r` but never smaller than `min`, centred and clamped to
/// `r`. Used by popups that size themselves to their content rather than to a percentage.
pub(super) fn centered_rect_with_min(
    percent_x: u16,
    percent_y: u16,
    min_width: u16,
    min_height: u16,
    r: Rect,
) -> Rect {
    let pct = |whole: u16, percent: u16| {
        u16::try_from(u32::from(whole) * u32::from(percent.min(100)) / 100).unwrap_or(u16::MAX)
    };
    let pct_w = pct(r.width, percent_x);
    let pct_h = pct(r.height, percent_y);
    let width = pct_w.max(min_width).min(r.width);
    let height = pct_h.max(min_height).min(r.height);
    Rect {
        x: r.x + (r.width - width) / 2,
        y: r.y + (r.height - height) / 2,
        width,
        height,
    }
}
