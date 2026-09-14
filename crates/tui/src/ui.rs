//! Drawing. The header box with its hint grid and stats block, the body, the prompt and status
//! lines, and the modal over the body.

use std::borrow::Cow;

use nutsh_catalog::{Column, FieldType, NAV};
use nutsh_core::cell::{Names, Rendered};
use nutsh_core::contexts::Header;
use nutsh_core::search::Query;
use nutsh_core::status;
use nutsh_core::store::TableKey;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, List, ListItem, ListState, Padding,
    Paragraph, Row, Table, TableState,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::DEFAULT_PORT;
use crate::app::{App, Live, Mode, View};
use crate::contexts::{self, Prompt};
use crate::detail::Detail;
use crate::page::PageView;
use crate::palette::{Entry, Slot, Tag};
use crate::{table, theme};
use nutsh_core::store::Failure;
// What a table or a pane says instead of rows when the endpoint answered 404. Not "the
// namespace is missing" and not "needs v4.3": the request was made and this is what came back.
// Defined beside `Availability::ListNotFound`, which is the same sentence about the same fact
// reached without a second request, so the greyed row and the table below it cannot drift.
use nutsh_prism::NOT_SERVED;

/// The header box's own height, borders included.
const HEADER_ROWS: u16 = 7;

/// The prompt and the status line, which both heights keep.
const FOOT_ROWS: u16 = 2;

/// The shortest frame that keeps the full header.
///
/// Derived from the tallest thing this app ever draws inside the body - the `?` overlay, at
/// [`HELP_BOX`]`.1` rows - plus the table chrome it is drawn over: two borders and a heading
/// row. Below that the overlay fills the body edge to edge and the frame behind it is gone,
/// which is precisely when it stops being an overlay; and a table showing fewer rows than the
/// header above it has is not a table with a header, it is a header with a footnote.
///
/// 16 + 3 + 7 + 2 = **28**. An eighty by twenty-four terminal - the one the product audit
/// measured, and the default size of a great many of them - is therefore folded, and the
/// design's own reference frame of a hundred by thirty is not.
pub const FULL_HEADER_ROWS: u16 = HELP_BOX.1 + 3 + HEADER_ROWS + FOOT_ROWS;

/// Whether this frame folds the header to one line: what the config file says, or - when it
/// says `auto`, which is what it says unless somebody changed it - whether the terminal is
/// shorter than [`FULL_HEADER_ROWS`].
fn compact(app: &App, height: u16) -> bool {
    match app.header {
        Header::Compact => true,
        Header::Full => false,
        Header::Auto => height < FULL_HEADER_ROWS,
    }
}

/// `[header 7 or 1, body, prompt 1, status 1]`. The old breadcrumb line is gone: it lives in the
/// header's `Kind:` field, which is where the seventh header line came from.
///
/// The folded header keeps the prompt and the status line, where §11.3 dropped the status line
/// too. The status line is where every refusal, every running task and every API error is
/// written, and folding it into a line that already carries five facts would make the message
/// the first thing cut on a narrow frame. Six rows of the seven are worth having; the seventh
/// is not worth a refusal nobody can read.
pub fn draw(app: &App, f: &mut Frame) {
    let area = f.area();
    // What the menu can afford is a property of the frame, and `handle` has no frame; the one
    // place every draw path passes through records it, so `App::menu_shown` answers the same
    // question for a key as for a pixel.
    app.width.set(area.width);
    // Cleared here and filled below by whatever draws. Every frame records afresh: a hit map
    // from the previous frame would answer for a layout that is no longer on screen.
    *app.hits.borrow_mut() = crate::mouse::Hits::default();
    let folded = compact(app, area.height);
    let [header, body, prompt, status] = Layout::vertical([
        Constraint::Length(if folded { 1 } else { HEADER_ROWS }),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    // The body's height, not the header's: the `Count:` field reports how many of a page's
    // panes this frame has room for, which is a property of the space below the header.
    if folded {
        draw_folded_header(app, f, header, body.height);
    } else {
        draw_header(app, f, header, body.height);
    }
    // The menu takes its width off the body only; what is left is what everything below draws
    // into. An overlay is drawn after the body, so it covers it.
    let mut overlay = None;
    let body = match menu(app, body) {
        Menu::Hidden => body,
        Menu::Split(w) => {
            let [side, rest] =
                Layout::horizontal([Constraint::Length(w), Constraint::Min(40)]).areas(body);
            draw_sidebar(app, f, side);
            rest
        }
        Menu::Overlay(w) => {
            overlay = Some(Rect {
                width: w.min(body.width),
                ..body
            });
            body
        }
    };
    // The palette, the picker, the form, the help overlay and the skin list are modals: what
    // they cover stays on screen. The Contexts screen is not a modal: it replaces the table,
    // since without a session there is no table.
    match app.background_mode() {
        Mode::Detail => draw_detail(app, f, body),
        Mode::Contexts | Mode::Form => draw_contexts(app, f, body),
        _ if app.page().is_some() => draw_page(app, f, body),
        _ => draw_table(app, f, body),
    }
    if let Some(side) = overlay {
        clear_region(f, side);
        draw_sidebar(app, f, side);
    }
    // The two dialogs that are as long as the kind's action list are allowed the header's rows
    // as well as the body's: a modal covers whatever is beneath it either way, and on a 100x30
    // terminal those seven rows are the difference between eighteen of a VM's twenty-five
    // actions and all of them.
    //
    // Only those two. The confirm is at most a prompt and five names, and centring it higher
    // would hide the column headings of the table it is asking about; the palette is anchored
    // to the `:` prompt it was typed at; the skins and the settings are a percentage of the
    // body; and the `?` overlay is a fixed box that
    // `help_draws_over_the_view_it_was_opened_from` measures against the view behind it.
    let dialog = Rect {
        y: header.y,
        height: header.height + body.height,
        ..body
    };
    match app.mode {
        Mode::Command => draw_palette(app, f, body),
        Mode::Skins => draw_skins(app, f, body),
        Mode::Settings => draw_settings(app, f, body),
        Mode::Picker => draw_picker(app, f, dialog),
        Mode::Menu => draw_menu(app, f, dialog),
        Mode::Confirm => draw_confirm(app, f, body),
        Mode::Fields => draw_fields(app, f, body),
        Mode::Journal => draw_journal(app, f, body),
        Mode::Search => draw_search(app, f, body),
        Mode::Form => draw_form(app, f, body),
        Mode::Help => draw_help(f, body),
        _ => {}
    }
    draw_prompt(app, f, prompt);
    // The folded header owns the freshness readout, so the status line gives up its cell for
    // it: one dot on the frame, and the flash and the meter get the sixteen columns back.
    draw_status(app, f, status, folded);
}

/// How the menu is shown at this width.
enum Menu {
    Hidden,
    /// Beside the body.
    Split(u16),
    /// Over the left of the body: below 90 columns a split would leave a 66-cell table, which
    /// is not a table.
    Overlay(u16),
}

fn menu(app: &App, area: Rect) -> Menu {
    if !app.menu_shown() {
        return Menu::Hidden;
    }
    // 24 normally, 20 when 24 would leave the body under the 40 cells it is laid out for.
    let width = if area.width >= 24 + 40 { 24 } else { 20 };
    if area.width >= crate::app::MENU_SPLIT_COLUMNS {
        Menu::Split(width)
    } else {
        Menu::Overlay(width)
    }
}

fn draw_sidebar(app: &App, f: &mut Frame, area: Rect) {
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
        Some(View::Table(v)) => l.store.table(&v.key).rows.len(),
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

/// The stats block's width, and the hint grid's: two 13-cell columns with a 2-cell gap.
const STATS_WIDTH: u16 = 26;
const HINTS_WIDTH: u16 = 28;
/// The narrowest info half worth having. Below `HINTS_WIDTH + INFO_MIN + 2` - a 70-cell box,
/// which with the 26-cell stats block beside it is a 96-column frame - the grid is dropped
/// and its hints move to the prompt line.
const INFO_MIN: u16 = 40;

fn draw_header(app: &App, f: &mut Frame, area: Rect, body: u16) {
    let [boxed, stats] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(STATS_WIDTH)]).areas(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(Span::styled(" nutsh ", theme::title()));
    let inner = block.inner(boxed);
    f.render_widget(block, boxed);
    // The Contexts screen goes without a grid rather than advertise `⏎ drill` where `enter`
    // connects; the table's and the page's own grids are in `hint_lines`.
    let grid = !app.on_contexts_screen() && inner.width >= HINTS_WIDTH + INFO_MIN;
    let [info, hints] = Layout::horizontal([
        Constraint::Min(INFO_MIN),
        Constraint::Length(if grid { HINTS_WIDTH } else { 0 }),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(info_lines(app, usize::from(info.width), body)),
        info,
    );
    if grid {
        f.render_widget(Paragraph::new(hint_lines(app)), hints);
    }
    f.render_widget(
        Paragraph::new(stats_lines(app)).alignment(Alignment::Right),
        stats,
    );
}

/// The whole header on one line: what this is, what the body is showing, which cluster the
/// session is scoped to, who is connected, whether it can change anything, and how fresh the
/// rows are.
///
/// The four facts the full box carries that nothing else on the frame does - context, cluster
/// scope, read-only, freshness - plus the breadcrumb, because a reader who cannot see the
/// `Kind:` field has no other way to tell which of two similar tables is open. What is dropped
/// with the six rows is the hint grid, which the prompt line and the `?` overlay both name in
/// full, and the stats block, which is a Prism Central's summary rather than this session's.
///
/// The freshness readout is right-aligned in its own cell, as it is on the status line, so it
/// sits at the same column in both heights and a reader's eye does not have to look for it.
fn draw_folded_header(app: &App, f: &mut Frame, area: Rect, body: u16) {
    let [left, sync] =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(SYNC_WIDTH)]).areas(area);
    const LEAD: &str = " nutsh ";
    /// What is never dropped to make room, only cut off by the backstop below.
    const KEEP: u8 = 3;
    // Everything after the elastic part, built first so the elastic part can be told what it
    // has left, and each tagged with what it is worth when the line cannot hold all of it.
    //
    // The count and the breadcrumb are on the table's own title bar as well, so they are the
    // cheap ones; the cluster scope, the context and `[read-only]` are on nothing else on the
    // frame, and `[read-only]` governs what a keystroke can do. Dropping by worth rather than
    // by position is what keeps the reading order the same at every width: the line loses
    // facts from the left, never from the end.
    let mut tail: Vec<(u8, Span<'static>)> = Vec::new();
    let head = match &app.live {
        // Without a session the same line says what there is to say, as `no_session_lines`
        // does: there is no context, no cluster and nothing polling.
        None => {
            tail.push((
                KEEP,
                Span::styled("  no session", Style::default().fg(theme::mauve())),
            ));
            app.config_path.clone().unwrap_or_else(|| "-".to_string())
        }
        Some(live) => {
            let s = &live.session;
            // Bracketed and labelled, where the box has a column of labels to lean on: a bare
            // `3` beside a bare `-` on one line is two facts a reader has to guess at, and a
            // frame this app is read from is a frame somebody has to be able to read cold.
            tail.push((
                0,
                Span::styled(format!("  [{}]", count_text(app, body)), theme::dim()),
            ));
            tail.push((1, Span::styled("  cluster:", theme::dim())));
            tail.push((
                1,
                Span::styled(
                    s.cluster
                        .as_ref()
                        .map_or("<all>", |c| c.name.as_str())
                        .to_string(),
                    Style::default().fg(theme::green()),
                ),
            ));
            tail.push((2, Span::styled("  ctx:", theme::dim())));
            tail.push((
                2,
                Span::styled(
                    s.context.clone().unwrap_or_else(|| "-".to_string()),
                    Style::default().fg(theme::mauve()),
                ),
            ));
            if s.readonly {
                tail.push((
                    KEEP,
                    Span::styled("  [read-only]", Style::default().fg(theme::red())),
                ));
            }
            if s.insecure {
                tail.push((
                    KEEP,
                    Span::styled("  [insecure]", Style::default().fg(theme::peach())),
                ));
            }
            breadcrumb(app)
        }
    };
    // The elastic part: the breadcrumb, or the config path when there is no session. Cut from
    // the **left**, because the tail of either is the half that says which one it is -
    // `…Storage › VMs`, `…/nutsh/config.toml`.
    let room = usize::from(left.width);
    let budget = room.saturating_sub(LEAD.width());
    // Cheapest first, until what is left fits. The breadcrumb has already given up everything
    // it has by then, since it takes only what the survivors leave.
    let width_of = |tail: &[(u8, Span<'static>)]| -> usize {
        tail.iter().map(|(_, s)| s.content.width()).sum()
    };
    for worth in 0..KEEP {
        if width_of(&tail) <= budget {
            break;
        }
        tail.retain(|(w, _)| *w > worth);
    }
    let mut spans = vec![
        Span::styled(LEAD, theme::title()),
        Span::styled(
            elide_left(&head, budget.saturating_sub(width_of(&tail))),
            Style::default().fg(if app.live.is_some() {
                theme::peach()
            } else {
                theme::sapphire()
            }),
        ),
    ];
    // The backstop, for a frame too narrow even for what `KEEP` marks: whole spans, and the
    // first that does not fit ends the line. One row is the whole point, so nothing may wrap.
    let mut used: usize = spans.iter().map(|s| s.content.width()).sum();
    for (_, span) in tail {
        let width = span.content.width();
        if used + width > room {
            break;
        }
        used += width;
        spans.push(span);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), left);
    let (label, colour) = sync_indicator(app);
    right_label(f, sync, &label, colour);
}

/// `{label:<12}` dim, then the value in the field's own colour.
fn field_line(label: &str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<12}"), theme::dim())];
    spans.extend(value);
    Line::from(spans)
}

fn info_lines(app: &App, width: usize, body: u16) -> Vec<Line<'static>> {
    let Some(live) = &app.live else {
        return no_session_lines(app, width);
    };
    let s = &live.session;
    // The default port is implied; anything else (the mock's ephemeral port, a proxy) is not.
    let endpoint = if s.port == DEFAULT_PORT {
        s.host.clone()
    } else {
        format!("{}:{}", s.host, s.port)
    };
    let mut context = vec![Span::styled(
        s.context.clone().unwrap_or_else(|| "-".to_string()),
        Style::default().fg(theme::mauve()),
    )];
    if s.readonly {
        context.push(Span::styled(
            "  [read-only]",
            Style::default().fg(theme::red()),
        ));
    }
    if s.insecure {
        context.push(Span::styled(
            "  [insecure]",
            Style::default().fg(theme::peach()),
        ));
    }
    let mut pc = vec![Span::styled(
        endpoint,
        Style::default().fg(theme::sapphire()),
    )];
    if let Some(v) = &s.pc_version {
        pc.push(Span::styled(
            format!("  {v}"),
            Style::default().fg(theme::sapphire()),
        ));
    }
    vec![
        field_line("Context:", context),
        field_line("PC:", pc),
        field_line(
            "Cluster:",
            vec![Span::styled(
                s.cluster
                    .as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "<all>".to_string()),
                Style::default().fg(theme::green()),
            )],
        ),
        field_line(
            "Kind:",
            vec![Span::styled(
                cut(&breadcrumb(app), width.saturating_sub(12)),
                Style::default().fg(theme::peach()),
            )],
        ),
        field_line(
            "Count:",
            vec![Span::styled(
                count_text(app, body),
                Style::default().fg(theme::text()),
            )],
        ),
    ]
}

/// Without a session the same five fields say what there is to say: which file the contexts
/// came from, and how many there are. An empty screen looks the same whether the config is
/// missing, elsewhere, or genuinely empty; the path says which.
fn no_session_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    vec![
        field_line(
            "Context:",
            vec![Span::styled(
                "no session",
                Style::default().fg(theme::mauve()),
            )],
        ),
        field_line(
            "Config:",
            vec![Span::styled(
                elide_left(
                    app.config_path.as_deref().unwrap_or("-"),
                    width.saturating_sub(12),
                ),
                Style::default().fg(theme::sapphire()),
            )],
        ),
        field_line(
            "Cluster:",
            vec![Span::styled("-", Style::default().fg(theme::green()))],
        ),
        field_line(
            "Kind:",
            vec![Span::styled(
                "Contexts",
                Style::default().fg(theme::peach()),
            )],
        ),
        field_line(
            "Count:",
            vec![Span::styled(
                contexts_count(app),
                Style::default().fg(theme::text()),
            )],
        ),
    ]
}

/// `N contexts`: what `Count:` says while the Contexts screen is on screen.
fn contexts_count(app: &App) -> String {
    let n = app.screen.rows.len();
    let plural = if n == 1 { "context" } else { "contexts" };
    format!("{n} {plural}")
}

/// `Group › Item › child`: the menu's path to what is open, then the drill-down.
fn breadcrumb(app: &App) -> String {
    let live = match &app.live {
        Some(live) if !app.on_contexts_screen() => live,
        _ => return "Contexts".to_string(),
    };
    let mut parts = Vec::new();
    if let Some(target) = &app.sidebar.open
        && let Some((group, label)) = nutsh_catalog::nav_path(target)
    {
        parts.push(group.to_string());
        parts.push(label.to_string());
    }
    for (i, view) in live.stack.iter().enumerate() {
        // A page names itself through the menu item above it, and its panes are named in
        // their own blocks; only the tables of the stack add to the path.
        let View::Table(view) = view else { continue };
        if let Some(parent) = &view.parent_name {
            parts.push(parent.clone());
        }
        // The first table is already named by the menu item above it - unless that item opened
        // a page, and this table came out of one of its panes.
        if i > 0 || parts.len() < 2 || view.parent_name.is_some() {
            parts.push(view.key.kind.display.to_string());
        }
    }
    parts.join(" › ")
}

/// `body` is the height of the area below the header, which is what decides how many of a
/// page's panes the frame has room for.
fn count_text(app: &App, body: u16) -> String {
    if app.on_contexts_screen() {
        return contexts_count(app);
    }
    // A page has as many row counts as it has panes, and each pane's title already carries
    // its own; what the field can say for the screen as a whole is how many there are - and,
    // on a frame too short for the whole grid, how many of them were drawn. That is the one
    // place the notice belongs: written over the body it would land on the first pane's
    // border and title.
    if let Some(page) = app.page() {
        let rows = page_rows(page.def, body);
        let drawn = page
            .panes
            .iter()
            .filter(|p| usize::from(p.def.row) < rows)
            .count();
        let total = page.panes.len();
        return if drawn == total {
            format!("{total} panes")
        } else {
            format!("{drawn} of {total} panes")
        };
    }
    let (Some(live), Some(view)) = (&app.live, app.view()) else {
        return "-".to_string();
    };
    count(live.store.table(&view.key), view.query())
}

/// `shown/total` when the server holds more rows than the table does, else `shown` - and
/// `matched of shown` in front of either while a `/` term stands. The header's `Count:` field
/// and the table title's `[count]` say the same thing.
///
/// The filtered form is `12 of 847`, and `847` is whatever the unfiltered field would have said,
/// `500/1200` included: the filter narrows what is **loaded**, so the loaded count is exactly
/// what it is a fraction of, and the server's own total goes on being the last number on the
/// line. A filtered table that read `[12]` would be indistinguishable from a table with twelve
/// rows in it, which is the one thing a count is there to prevent.
///
/// The staged total first, then `total`: `Table::total` is the *last completed* cycle's count and
/// stays `None` until a `Complete` stages, so a table walking its first cycle would show a bare
/// `0` for the whole walk and the server's own count would arrive only once it stopped mattering.
/// A cycle in flight has the newer number, and a table a previous cycle emptied would otherwise be
/// pinned at that cycle's `0` for the whole of the next walk. Invisible to a settled table -
/// `Complete`, `Error` and `abandon` all drop the staging - so no existing snapshot moves.
fn count(t: &nutsh_core::store::Table, filter: Option<&Query>) -> String {
    let loaded = match t.staged_total().or(t.total) {
        Some(total) if u64::try_from(t.rows.len()).is_ok_and(|n| total > n) => {
            format!("{}/{total}", t.rows.len())
        }
        _ => t.rows.len().to_string(),
    };
    match filter {
        None => loaded,
        Some(query) => format!("{} of {loaded}", table::matching(t, Some(query))),
    }
}

/// What a table with no rows says once nothing is in flight: the last cycle's error, else the
/// caller's own `empty` text.
///
/// A claim about the server's answer waits for one: `Store::table` hands back a fresh
/// placeholder for a key no cycle has completed, and "no rows" on the frame a table opens on
/// would be asserting what has not been asked yet.
///
/// Split out of [`body_note`] because a pane draws this half inside its own block, in place of
/// the chrome, while the in-flight half goes through `render_rows` under the column headings -
/// two render sites, one rule about which sentence is true.
///
/// The 404 goes in front of the error, so the sentence is the specific one: `not_served` is set
/// by the same cycle that wrote `t.error`, and the two would otherwise race to say it.
///
/// `Failure::text`, never the error as it arrived: the far end's own sentence is kept on the
/// `Failure` for `:journal` and goes no further. A table is not where a person is told to check
/// their spelling.
fn settled_note<'a>(t: &'a nutsh_core::store::Table, empty: &'a str) -> Option<Cow<'a, str>> {
    if t.not_served {
        return Some(Cow::Borrowed(NOT_SERVED));
    }
    match &t.error {
        Some(failure) => Some(Cow::Owned(failure.text())),
        None => t.last_poll.is_some().then_some(Cow::Borrowed(empty)),
    }
}

/// What the body says when it has no rows to draw.
///
/// One of the three places progress is stated, and the only one that says what is happening
/// *right now*: the title carries the numbers and the header's marker says what the table is.
/// The cycle in flight is tested **before** the error, and that order is the point of the
/// function: `Update::Started` does not clear `Table::error` - only a `Complete` does - so a
/// retry after a failure would otherwise draw a title whose clock ticks beside a body still
/// reciting the previous cycle's failure. The error is not lost: `error_text` keeps it on the
/// status line in red for as long as it stands.
///
/// `budget` is what the *subscription* asked for, not what the kind allows: a pane's walk is
/// bounded by `Subscription::sized`, which replaces `Kind::max_rows` with the pane's own
/// height, and promising the kind's 500 in a pane that will stop at 20 is a wrong number on
/// exactly the frame this sentence exists for.
fn body_note<'a>(
    t: &'a nutsh_core::store::Table,
    budget: Option<u32>,
    stopped: bool,
    empty: &'a str,
) -> Option<Cow<'a, str>> {
    if !t.rows.is_empty() {
        return None;
    }
    if t.cycle_started.is_some() {
        return Some(Cow::Owned(match (t.staged(), t.staged_total()) {
            // From the catalog, with **no server round trip**, so it is on the very first frame:
            // `500` for audits. A walk with no budget at all - only ever a hand-built test
            // `Kind` - says `listing…`.
            (0, _) => match budget {
                Some(max) => format!("listing up to {max} rows…"),
                None => "listing…".to_string(),
            },
            (staged, Some(total)) => format!("listing… {staged} of {total} rows"),
            (staged, None) => format!("listing… {staged} rows"),
        }));
    }
    // `ctrl-x` before the first page staged lands nothing, so falling through to `no rows`
    // would be a claim about a collection nobody looked at.
    if stopped && t.error.is_none() && !t.not_served {
        return Some(Cow::Borrowed("stopped before the first page"));
    }
    debug_assert!(
        body_states_the_failure(t),
        "the two gates above are `body_states_the_failure`, which the status line reads"
    );
    settled_note(t, empty)
}

/// ` [↻ {staged} rows · {elapsed}]`, dim, while a listing cycle is in flight; nothing otherwise.
///
/// Before the first page lands `staged` is `0` and the bracket still appears - that is the point.
/// Numbers, never a glyph: the header's one-glyph vocabulary says what the table *is*, and two
/// vocabularies fighting over one fact is how a frame stops being read.
///
/// The one span on the frame measured against the wall clock rather than `App::now`: an elapsed
/// time needs a monotonic clock, `App::now` is a `SystemTime`, and a `SystemTime` cannot measure
/// one. `Table::cycle_started` holds the `Instant` the cycle began, and this reads it.
fn walking(t: &nutsh_core::store::Table) -> Option<Span<'static>> {
    let (_, started) = t.cycle_started?;
    Some(Span::styled(
        format!(
            " [↻ {} rows · {}]",
            t.staged(),
            nutsh_core::cell::span(started.elapsed().as_secs())
        ),
        theme::dim(),
    ))
}

/// Five rows of two 13-cell cells: `{key:>2} {label:<10}`, key sky bold, label dim.
///
/// Per view, because the keys are: a page sorts and widens nothing, and a table has no panes.
/// Every pair names a key the view actually binds, and the grid is full: five rows is what the
/// seven-line header box holds beside five `info_lines`, and two 13-cell cells is what 28 cells
/// hold. So `a actions` - the key to the twenty-five actions a VM has, and the one thing here
/// no other surface said - cost `^b menu` its cell. `^b:menu` is on the prompt line at every
/// width and toggles a pane that is on screen; `a` was on nothing and opens what is not.
///
/// §11.2's grid offers `a actions` on a *page* too, and it is there now that `handle_page`
/// binds it: the whole action pipeline reads `App::cursor_table`, which answers for a focused
/// pane as it does for a table view. It cost the page's grid `^b menu` for the reason it cost
/// the table's - `^b:menu` is on the prompt line at every width and toggles a pane that is on
/// screen, where `a` was on nothing and opens what is not.
fn hint_lines(app: &App) -> Vec<Line<'static>> {
    const TABLE: [[(&str, &str); 2]; 5] = [
        [("⏎", "drill"), ("y", "detail")],
        [("S", "sort"), ("w", "wide")],
        [("←→", "columns"), ("^r", "refresh")],
        [("^t", "every"), ("^x", "stop")],
        [("a", "actions"), ("^c", "quit")],
    ];
    const PAGE: [[(&str, &str); 2]; 5] = [
        [("⏎", "detail"), ("O", "open table")],
        [("⇥", "pane"), ("y", "detail")],
        [("jk", "move"), ("^r", "refresh")],
        [("^t", "every"), ("^x", "stop")],
        [("a", "actions"), ("^c", "quit")],
    ];
    let hints = if app.page().is_some() { &PAGE } else { &TABLE };
    hints
        .iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (i, (key, label)) in row.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(
                    format!("{key:>2} "),
                    Style::default()
                        .fg(theme::sky())
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(format!("{label:<10}"), theme::dim()));
            }
            Line::from(spans)
        })
        .collect()
}

/// Seven lines, right-aligned in 26 cells, the first and the sixth blank: the four counters
/// sit level with `Context:`..`Kind:` and the version line with the box's bottom border, as
/// in §11.1. Labels dim, numbers text, `on` green and `off` red, `⚠`/`✖` yellow and red and
/// both dim at zero so a quiet Prism Central looks quiet, `▶ N running` peach above zero, and
/// the version line dim with the PC version sapphire. The version line is `v0.0.2-beta1 ·
/// pc.7.6`: this program's version and the Prism Central's, in that order, and neither named
/// because the box beside them says which is which.
fn stats_lines<'a>(app: &'a App) -> Vec<Line<'a>> {
    use nutsh_core::stats::{Count, Stats, show};
    // Assembled at compile time: the one label that is not a literal is the same on every frame.
    // No program name: the box's own title, two cells to the left on the same frame, already
    // reads `nutsh`. Naming it twice on one line was affordable while the version was five
    // characters and is not at eleven, and this block is twenty-six cells wide - the six that
    // buys are what keeps the Prism Central's version from being clipped off the end.
    const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"), " · ");
    let empty = Stats::default();
    let s = app.live.as_ref().map_or(&empty, |l| l.store.stats());
    // A stale block is dim throughout, so a screen of numbers never looks live when it is not.
    let plain = Style::default().fg(if s.stale {
        theme::overlay1()
    } else {
        theme::text()
    });
    let tinted = |colour| Style::default().fg(if s.stale { theme::overlay1() } else { colour });
    let label = |t: &'static str| Span::styled(t, theme::dim());
    let value = |t: String, style: Style| Span::styled(t, style);
    let warn = |c: Count, colour| {
        Style::default().fg(if c.unwrap_or(0) == 0 || s.stale {
            theme::overlay1()
        } else {
            colour
        })
    };
    vec![
        Line::from(""),
        Line::from(vec![
            label("clusters "),
            value(show(s.clusters), plain),
            label("  hosts "),
            value(show(s.hosts), plain),
        ]),
        Line::from(vec![
            label("vms "),
            value(show(s.vms), plain),
            label("  on/off "),
            value(show(s.vms_on), tinted(theme::green())),
            label("/"),
            value(show(s.vms_off()), tinted(theme::red())),
        ]),
        Line::from(vec![
            label("alerts "),
            value(
                format!("⚠ {}", show(s.alerts_warning)),
                warn(s.alerts_warning, theme::yellow()),
            ),
            label("  "),
            value(
                format!("✖ {}", show(s.alerts_critical)),
                warn(s.alerts_critical, theme::red()),
            ),
        ]),
        Line::from(vec![
            label("tasks "),
            value(
                format!("▶ {} running", show(s.tasks_running)),
                warn(s.tasks_running, theme::peach()),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            label(VERSION),
            Span::styled(
                app.live
                    .as_ref()
                    .and_then(|l| l.session.pc_version.as_deref())
                    .unwrap_or("-"),
                Style::default().fg(theme::sapphire()),
            ),
        ]),
    ]
}

/// A text value with its cursor, as spans. `cursor` is a byte offset into `text`, or `None` for a
/// value that is not typed into (a checkbox, an inactive field).
///
/// At the end of the text the cursor is a `█` in `colour`, exactly as it has always been drawn -
/// which is what lets a text snapshot assert a ghost by the characters *after* the block. Inside
/// the text it is the **whole cluster** it stands on, painted `fg(base) on colour`: the same
/// cursor, the same colours, and nothing hidden. `theme::selected_row()` inverts a row the same
/// way, so the two cursors on a frame read alike.
///
/// The cluster, not the character: `e` followed by U+0301 is two scalars and one cell, and
/// ratatui drops a zero-width grapheme that *begins* a span - so a reversed cell covering only
/// the `e` would delete the accent from the frame, which is precisely what this function's
/// contract says it must not do. [`crate::text::cluster_end`] is the one rule, shared with
/// `Input::left`/`right`, so the cursor's idea of a cell and the frame's cannot drift.
fn cursor_spans<'a>(
    text: &'a str,
    cursor: Option<usize>,
    style: Style,
    colour: Color,
) -> Vec<Span<'a>> {
    let Some(cursor) = cursor else {
        return vec![Span::styled(text, style)];
    };
    let mut spans = vec![Span::styled(&text[..cursor], style)];
    if cursor >= text.len() {
        spans.push(Span::styled("█", Style::default().fg(colour)));
        return spans;
    }
    let end = crate::text::cluster_end(text, cursor);
    spans.push(Span::styled(
        &text[cursor..end],
        Style::default().fg(theme::base()).bg(colour),
    ));
    spans.push(Span::styled(&text[end..], style));
    spans
}

/// [`cursor_spans`] over a value the caller owns rather than borrows - a masked password, a
/// checkbox, an enum's word - where there is nothing in the widget to point at. The prompt line
/// keeps the borrowing version, because its text *is* the widget's.
fn owned_cursor_spans(
    text: &str,
    cursor: Option<usize>,
    style: Style,
    colour: Color,
) -> Vec<Span<'static>> {
    cursor_spans(text, cursor, style, colour)
        .into_iter()
        .map(|s| Span::styled(s.content.into_owned(), s.style))
        .collect()
}

/// The per-mode hint string in dim, or the active input. On a frame too narrow for the header's
/// hint grid this is where the long hint string goes.
fn draw_prompt(app: &App, f: &mut Frame, area: Rect) {
    /// The typed text between its lead and its cursor, borrowed: it is redrawn every frame.
    /// `elided` prefixes a dim `…` for a line that scrolled off the left (design §8.4), and
    /// `ghost` is the rest of the best match, offered after the block.
    fn input<'a>(
        lead: &'static str,
        text: &'a str,
        cursor: usize,
        ghost: &'a str,
        elided: bool,
    ) -> Line<'a> {
        let mut spans = vec![Span::styled(
            lead,
            Style::default()
                .fg(theme::mauve())
                .add_modifier(Modifier::BOLD),
        )];
        if elided {
            spans.push(Span::styled("…", theme::dim()));
        }
        spans.extend(cursor_spans(
            text,
            Some(cursor),
            Style::default().fg(theme::text()),
            theme::mauve(),
        ));
        // After the block, never before it: the block marks the boundary between what was typed
        // and what is offered, which is also how a text snapshot asserts a ghost - `:subn█et`.
        if !ghost.is_empty() {
            spans.push(Span::styled(ghost, theme::dim()));
        }
        Line::from(spans)
    }
    // The picker's filter and the menu's are typed without a prefix, so they are shown without
    // one: the hint that says `type to filter` stays until the first letter, then makes way
    // for it.
    let typed = match app.mode {
        Mode::Picker => app.picker.as_ref().map(|p| p.input.as_str()),
        Mode::Menu => app.menu.as_ref().map(|m| m.input.as_str()),
        // The form itself types into its fields, where the text already shows; only the
        // reference picker over it has a filter with nowhere else to go.
        Mode::Fields => app
            .form
            .as_ref()
            .and_then(|f| f.picker.as_ref())
            .map(|p| p.input.as_str()),
        _ => None,
    }
    .filter(|text| !text.is_empty());
    let line = match (app.mode, typed) {
        (Mode::Command, _) => match app.palette.as_ref() {
            Some(p) => {
                // One cell for the `:` lead. The guard only exists for the palette: a filter
                // prompt is short, has never had one, and adding one would re-record its five
                // snapshots for a case that cannot happen.
                //
                // The room is measured on the *typed* text alone, so on a line long enough to
                // elide the ghost is what ratatui clips: the offer disappears rather than the
                // window scrolling to keep it. Whoever widens this guard owns that too.
                let (elided, start) = p.input.window(usize::from(area.width).saturating_sub(1));
                input(
                    ":",
                    &p.input.as_str()[start..],
                    p.input.cursor() - start,
                    p.ghost().unwrap_or(""),
                    elided,
                )
            }
            None => input(":", "", 0, "", false),
        },
        // The table's `/` term, with its own lead and a real cursor in it: unlike the three
        // filters below it is edited rather than typed and cleared, so a typo in the middle of
        // an address costs one `←` and not the whole line.
        (Mode::Table, _) if app.filtering() => match app.view().and_then(|v| v.filter.as_ref()) {
            Some(f) => input("/", f.input().as_str(), f.input().cursor(), "", false),
            None => input("/", "", 0, "", false),
        },
        // A filter is typed and cleared, never edited, so its block stays at the end, and it
        // completes nothing: the three filters rank no list to take a remainder from.
        (_, Some(text)) => input(" ", text, text.len(), "", false),
        // The body's keys give the line up to a task this session started, and take it back when
        // the last watch finishes. What a key has to say is on the status line under this one,
        // so neither ever hides the other.
        //
        // Only the body's: a modal's hint is the one place its keys are named - `y run` on a
        // confirm, `tab next` on a form, `esc close` on the journal - and a watch holds this
        // line for up to fifteen minutes, which is exactly when the next action is being
        // chained. The table's and the pane's keys are on the help overlay and in the header's
        // hint grid, so those two can lend the line out.
        (Mode::Table | Mode::Detail, _) => match task_line(app) {
            Some(text) => Line::from(Span::styled(
                format!(" {text}"),
                Style::default().fg(theme::subtext0()),
            )),
            None => Line::from(Span::styled(hint_text(app), theme::dim())),
        },
        _ => Line::from(Span::styled(hint_text(app), theme::dim())),
    };
    f.render_widget(Paragraph::new(line), area);
}

/// The progress of the task this session is following, or how many it is following when there
/// is more than one. `None` when none is running, which is when the hints come back.
fn task_line(app: &App) -> Option<String> {
    let mut running = app.live.as_ref()?.tasks.running();
    let first = running.next()?;
    match running.count() {
        0 => Some(first.line()),
        rest => Some(format!("{} tasks running", rest + 1)),
    }
}

/// Each string carries its own leading space, so the prompt line is one borrowed span.
///
/// The line names the body's keys and does not change with the focus - the promise `tab:focus`
/// has always made. With the menu focused, `:`, `^b`, `S`, `w` and `?` still reach the body
/// through `Sidebar::Action::Passthrough`, `⇥` and `esc` come back to it, and `⏎` opens the
/// highlighted menu entry. So `esc:menu` names the gesture - `esc` is the way between the two
/// panes - rather than claiming which side you are standing on. Saying it per focus would
/// double every `Mode::Table` arm to change one word, and the menu's own keys - `/`, `h`/`l`,
/// the digits - are not on this line from either side.
fn hint_text(app: &App) -> &'static str {
    match app.mode {
        // A page runs in `Mode::Table` and binds a smaller set: `tab` cycles its panes rather
        // than focusing the menu, and it neither sorts nor widens. Its `esc` is always the
        // menu: `App::open_page` drains the stack, so a page on top of it is its root - an
        // invariant that method debug-asserts, so the collapsed arm keeps a test-time guard.
        // `O:open table` gives up six cells to `a:actions`, which the grid spells in full
        // beside it: the line is 73 cells, under the 75 an 80-column terminal holds.
        Mode::Table if app.page().is_some() => {
            " :palette  ^b:menu  ⇥:pane  O:open  a:actions  ⏎:detail  esc:menu  ?:help"
        }
        // `y:detail` gives up its place to `esc`: the header's hint grid carries `y detail`,
        // and `esc` is the key whose meaning changes with where you are, so it is the one the
        // line has to spell out. `w:wide` gives up its place to `a:actions` for the same
        // reason the grid's `^b menu` did: both the grid and the `?` overlay name `w`, and
        // nothing named `a`. `S:sort` then gives up its place to `/:filter` on the same terms
        // and for a stronger reason: `S` is on the grid and in the `?` overlay, `/` was on
        // neither, and the line is the one surface a narrow terminal still has - which is
        // exactly where scrolling a thousand rows is least affordable. `tab` is spelled `⇥`,
        // as the page's line already spells it, to pay for the two cells. The line is 75
        // cells, which is what an 80-column terminal - the narrowest the tests render - holds;
        // a 76th would be clipped, not wrapped, and the key it took away would be `?:help`.
        Mode::Table if app.at_root() => {
            " :palette  ^b:menu  ⇥:focus  /:filter  a:actions  ⏎:drill  esc:menu  ?:help"
        }
        Mode::Table => {
            " :palette  ^b:menu  ⇥:focus  /:filter  a:actions  ⏎:drill  esc:back  ?:help"
        }
        // `PgUp/PgDn:page` gives up its place to `a:actions`: the pane now lists what can be
        // done to the row, and a list nothing says how to run is half a feature. The paging
        // keys keep their line on the `?` overlay, which is where they were learned anyway.
        Mode::Detail => " j/k:scroll  g/G:top/bottom  a:actions  Y:yaml  J:json  esc:back",
        Mode::Command => "",
        Mode::Skins => " enter apply  esc close",
        Mode::Settings => " space:toggle  enter:choose  a:add  d:remove  esc:close",
        // The box lists both groups, so the one key it binds does both things: `enter` opens a
        // related kind and runs an action, and which it did is never a surprise.
        Mode::Picker => " type to filter  enter open or run  esc close",
        Mode::Menu => " type to filter  ↑↓ move  enter run  esc close",
        // A type-name confirm is the one that wants typing; a yes/no one must not advertise it.
        Mode::Confirm if app.confirm.as_ref().is_some_and(|c| c.expect.is_some()) => {
            " type what the prompt asks for  enter confirm  esc cancel"
        }
        Mode::Confirm => " y run  any other key cancels",
        // The picker over a reference field is a list, not a form: it binds its own keys.
        Mode::Fields if app.form.as_ref().is_some_and(|f| f.picker.is_some()) => {
            " type to filter  ↑↓ move  enter choose  esc back"
        }
        Mode::Fields => " tab next  space toggle  ←→ move/cycle  ^w word  enter submit  esc cancel",
        // Without a session there is no table behind the rows, so `esc` has nowhere to go and
        // promising it a way back is a lie the user only finds out by pressing it.
        Mode::Contexts if app.live.is_none() => {
            " enter connect  a add  l login  d remove  ^r reload  q quit"
        }
        Mode::Contexts => " enter connect  a add  l login  d remove  ^r reload  esc back  q quit",
        Mode::Form => " tab next  shift-tab previous  space toggle  enter submit  esc cancel",
        Mode::Help => " esc close",
        Mode::Journal => " j k g G scroll  esc close",
        Mode::Search => " j k move  enter open  esc close",
    }
}

/// `[Min(10), Length(METER_WIDTH), Length(SYNC_WIDTH)]`: the flash left, the meter, the sync
/// indicator right. The meter's cell is fixed so that a long flash cannot push it into the
/// indicator, and it is right-aligned inside it so the two readouts read as one block at the
/// right-hand end of the line.
///
/// The readout is taken *before* the layout so its cell can collapse to nothing when there is
/// none - a `--snapshot` run, an app with no session, any frame under 80 columns. Those frames
/// are exactly the ones that need the width elsewhere: the flash keeps every column it had
/// before the meter existed, so a connect error or a table's API error is cut where it always
/// was, and the sync indicator keeps its sixteen columns on a frame too narrow to seat three
/// cells (three cells need 52 columns to be drawn whole, two need 26).
fn draw_status(app: &App, f: &mut Frame, area: Rect, folded: bool) {
    let readout = meter_readout(app, area.width);
    let [flash, meter, sync] = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(if readout.is_some() { METER_WIDTH } else { 0 }),
        Constraint::Length(if folded { 0 } else { SYNC_WIDTH }),
    ])
    .areas(area);
    let error = error_text(app);
    let (text, style) = match (app.status.as_deref(), error.as_deref()) {
        (Some(status), _) => (status, Style::default().fg(theme::subtext0())),
        (None, Some(error)) => (error, Style::default().fg(theme::red())),
        (None, None) => ("", theme::dim()),
    };
    let room = usize::from(flash.width).saturating_sub(1);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", style),
            Span::styled(cut(text, room), style),
        ])),
        flash,
    );
    if let Some((label, colour)) = readout {
        right_label(f, meter, &label, colour);
    }
    if !folded {
        let (label, colour) = sync_indicator(app);
        right_label(f, sync, &label, colour);
    }
}

/// One right-aligned label in its own cell: the meter and the sync indicator are the same
/// widget twice, and the alignment is what makes the two read as one block at the end of the
/// line however wide the frame is.
fn right_label(f: &mut Frame, area: Rect, label: &str, colour: Color) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(label, Style::default().fg(colour))))
            .alignment(Alignment::Right),
        area,
    );
}

/// `99.9 req/s · 100% cached` - the widest text there is, because `Meter::rate` is clamped at
/// 99.9 and a ratio at 100% - is 24 columns; the cell is that, plus the space that aligns it
/// with the sync indicator's own trailing space, plus one column of gutter so that even the
/// widest readout keeps a space between itself and the flash.
const METER_WIDTH: u16 = 26;

/// The sync indicator's cell. `◌ cached 12m ` is thirteen columns - the widest of the three
/// states - with three spare for an age that runs to `100d`.
const SYNC_WIDTH: u16 = 16;

/// The meter as the status line draws it, or `None` when there is nothing to say: no session
/// (nothing has been asked of anything, exactly as `sync_indicator` reads empty), a
/// `--snapshot` run, or a frame too narrow for even the rate.
///
/// Amber when the last ten seconds were paced - the local budget holding `acquire` back, or a
/// 429 the far end served, which drain the same bucket - and red when the pipe failed in the
/// same window. Red beats amber: a broken connection is the more useful of the two facts.
fn meter_readout(app: &App, width: u16) -> Option<(String, Color)> {
    if app.one_frame_run {
        return None;
    }
    let live = app.live.as_ref()?;
    let meter = live
        .session
        .client
        .metrics()
        .snapshot(std::time::Instant::now());
    let text = nutsh_prism::meter_text(&meter, width)?;
    let colour = if meter.failed {
        theme::red()
    } else if meter.throttled {
        theme::peach()
    } else {
        theme::subtext0()
    };
    Some((format!("{text} "), colour))
}

/// What the flash reports in red when no status claims the line: the open detail's own error
/// first - one missing entity never marks its table failed, so the pane is the only place it
/// is recorded - else the current table's last error, or, on a page, the focused pane's.
///
/// A pane that already holds rows keeps drawing them when a later cycle fails, so without this
/// the failure would be nowhere on the frame: the pane's own block only says a reason instead
/// of rows while it has none.
///
/// Which is exactly the gate, and it was missing: a table or a pane the body is already
/// stating the failure for had it repeated down here, so one frame carried the same failure
/// twice. [`body_states_the_failure`] is the justification above, applied.
fn error_text(app: &App) -> Option<String> {
    let live = app.live.as_ref()?;
    if let Some(failure) = app.detail.as_ref().and_then(|d| d.error.as_ref()) {
        return Some(failure.text());
    }
    let unstated = |key| {
        let t = live.store.table(key);
        (!body_states_the_failure(t)).then(|| t.error.as_ref().map(Failure::text))?
    };
    app.view()
        .and_then(|view| unstated(&view.key))
        .or_else(|| unstated(&app.page()?.focused()?.key))
}

/// Whether the body is already saying why: [`body_note`] draws [`settled_note`] exactly when
/// the table has no rows **and** no cycle in flight, and this is that condition, named, so the
/// status line can keep out of the body's way without the two gates drifting apart. The
/// `debug_assert` in `body_note` is what holds them together.
///
/// Both halves matter. A table with rows keeps drawing them when a cycle fails, so the status
/// line is the only place the failure can appear; and a table with none whose *next* cycle is
/// already in flight says `listing…` where the sentence was, so the status line is again the
/// only place it is. In between - settled, empty, failed - the body has it, and repeating it
/// put the same failure on one frame twice, once in the reader's words and once in the
/// gateway's.
fn body_states_the_failure(t: &nutsh_core::store::Table) -> bool {
    t.rows.is_empty() && t.cycle_started.is_none()
}

/// `◌ cached 12m` for rows the cache painted and nothing has refreshed yet, `● live` when the
/// current table's last cycle completed, `○ syncing` while it is loading or backing off. The
/// detail view answers the same way: the document is refreshed by a subscription of its own on
/// the table's rhythm (`App::open_detail`), so it is as live as the table under it, and its own
/// fetch failing is a `○ syncing` too. Without a session nothing polls, and the indicator says
/// nothing. Ahead of all of them, and returned before any of the logic below is reached:
/// `⏸ idle` while the scheduler is paused, since then nothing is being refreshed at all.
///
/// `◌ cached` is deliberately *not* `○ syncing`: "syncing" understates it. The rows on screen
/// are real, they are simply old, and the age is the only thing worth saying. `○ syncing`
/// keeps its meaning for a table with nothing in it. No change to the existing predicate was
/// needed to get here - `Table::restored_at` is cleared by the cycle that replaces the rows, and
/// `● live` already required `last_poll.is_some()`.
fn sync_indicator(app: &App) -> (String, Color) {
    let Some(live) = app.live.as_ref() else {
        return (String::new(), theme::overlay1());
    };
    // A walk stopped by hand, tested before everything else. It has to come before `● live`:
    // a stopped table has completed a cycle and has rows, so without a state of its own it
    // would read `● live` about a truncation the user asked for. A distinct glyph from
    // `⏸ idle` because the two are different facts - idle is the program's own economy,
    // stopped is the user's instruction - and one glyph for both would make `ctrl-r` mean two
    // things.
    if app.stopped() {
        return ("⏹ stopped ".to_string(), theme::peach());
    }
    // Nothing is polling this view and nothing will until it is asked: a different fact from
    // `⏹ stopped`, which is a walk truncated and a schedule still standing, and from `⏸ idle`,
    // whose promise that any keystroke brings the polling back is false here. The view's own
    // silence outranks the session's, which is why this is tested above the pause.
    if app.manual() {
        return ("⏹ manual ".to_string(), theme::overlay1());
    }
    // Nothing is polling, so nothing on screen is being refreshed; saying `● live` here would
    // be the indicator's first lie.
    if live.scheduler.is_idle() {
        return ("⏸ idle ".to_string(), theme::overlay1());
    }
    // A page is live when every pane that polls has completed a cycle: one pane still on its
    // first fetch is a screen still filling in. It is `◌ cached` when a pane is still showing
    // the cache's rows, dated by the oldest of them, since that is the age of the screen.
    if let Some(page) = app.page() {
        let detail_failing = app.detail.as_ref().is_some_and(|d| d.error.is_some());
        let polling = || page.panes.iter().filter(|p| p.sub.is_some());
        let settled = !detail_failing
            && polling().all(|p| {
                let t = live.store.table(&p.key);
                t.error.is_none() && !t.loading && t.last_poll.is_some()
            });
        if settled {
            return ("● live ".to_string(), theme::green());
        }
        let oldest = polling()
            .filter_map(|p| live.store.table(&p.key).restored_at)
            .min();
        return match oldest.filter(|_| !detail_failing) {
            Some(at) => (cached_label(app, at), theme::overlay1()),
            None => ("○ syncing ".to_string(), theme::yellow()),
        };
    }
    let Some(view) = app.view() else {
        return (String::new(), theme::overlay1());
    };
    let t = live.store.table(&view.key);
    let detail_failing = app.detail.as_ref().is_some_and(|d| d.error.is_some());
    if let Some(at) = t.restored_at.filter(|_| !detail_failing) {
        return (cached_label(app, at), theme::overlay1());
    }
    if t.error.is_none() && !t.loading && t.last_poll.is_some() && !detail_failing {
        let (every, source) = app.refresh_of(view.key.kind);
        let suffix = match (every, source) {
            // The catalog's own rhythm says nothing. `↻` and not a bare number, because
            // `◌ cached 12m`
            // already puts a length of time in this cell and means *twelve minutes old*.
            (Some(_), nutsh_core::contexts::Source::Catalog) | (None, _) => String::new(),
            (Some(d), _) => format!(" ↻{}", nutsh_core::cell::span(d.as_secs())),
        };
        (format!("● live{suffix} "), theme::green())
    } else {
        ("○ syncing ".to_string(), theme::yellow())
    }
}

/// `◌ cached 12m `, the age spelled the way every other age on the screen is: `cell::age` is
/// what the journal's `WHEN` and every `Timestamp` column use, and two spellings of "how long
/// ago" on one frame is one too many. A clock behind the write reads `0s` rather than running
/// backwards.
fn cached_label(app: &App, at: u64) -> String {
    let written = std::time::UNIX_EPOCH + std::time::Duration::from_secs(at);
    format!("◌ cached {} ", nutsh_core::cell::age(written, app.now))
}

/// The last `room` columns of `path`, marked with a leading `…` when something was dropped.
/// A path is recognised by its end - the file name, and the directory above it - so clipping
/// it from the right, which is what the terminal would do, throws away the half that
/// identifies it.
fn elide_left(path: &str, room: usize) -> String {
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

fn draw_table(app: &App, f: &mut Frame, area: Rect) {
    let (Some(live), Some(view)) = (&app.live, app.view()) else {
        return;
    };
    let t = live.store.table(&view.key);
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
            all: table::columns(view.key.kind, view.wide),
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

/// One table's rows, as the view or the pane that owns them describes itself.
struct Rows<'a> {
    key: &'a TableKey,
    /// `None` for the table view; the pane's index in `PageView::panes` otherwise. It rides
    /// into the hit map, which is what lets a click name the pane it landed on rather than the
    /// position that pane happened to be drawn in.
    pane: Option<usize>,
    /// Every column it could show, before the column scroll and the drop rule.
    all: &'static [Column],
    col_offset: usize,
    sort: Option<(usize, bool)>,
    selected: usize,
    /// The extIds `space` has marked, for the table view; `None` for a page pane, which marks
    /// nothing.
    marks: Option<&'a std::collections::HashSet<String>>,
    /// Whether the selection is drawn at all: a table view always marks its row and so does
    /// the page's current pane, while the panes beside it do not - the frame says which rows
    /// the cursor keys would move, and only one pane's would move.
    cursor: bool,
    /// Whether the border is the focused one.
    focused: bool,
    /// What to draw instead of rows when there are none: `listing up to 500 rows…`, the pane's
    /// own `empty` text, or the cycle's error. [`body_note`] decided both what it says *and*
    /// that there are no rows to say it in place of, so `render_rows` draws whatever it is
    /// handed.
    note: Option<Cow<'a, str>>,
    /// What `/` has narrowed the view to. `None` for a page's pane, which has no `/`: the key
    /// belongs to the table the body shows, and a pane is not one.
    filter: Option<&'a Query>,
    /// The title spans before the ` ‹n »n ` the column scroll appends.
    title: Vec<Span<'static>>,
}

/// The mark `space` puts in front of a marked row's first cell.
const MARK: &str = "* ";

/// The rows of one table, in the table chrome: the drop rule, the width waterfall, the row
/// tints and the selection bar. The table view and every page pane go through it, so a pane
/// row is a table row in the strongest sense - the same function drew it.
fn render_rows(app: &App, live: &Live, spec: Rows<'_>, f: &mut Frame, area: Rect) {
    let t = live.store.table(spec.key);
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
            let style = if Some(i) == status {
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

/// The one line a body draws instead of rows: dim, and indented to where a row would start,
/// because the rows of a table or a pane that has any are drawn past the selection gutter and
/// the note takes their place.
///
/// Both render sites go through it - `render_rows` under the column headings, `draw_pane`
/// inside its own block - so the sentence is placed and coloured once.
fn note_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("{}{text}", " ".repeat(table::GUTTER.width())),
        theme::dim(),
    ))
}

/// The rounded block a table and a pane share, so a pane that has rows and a pane that says
/// why it has none are framed identically.
fn table_block(mut title: Vec<Span<'static>>, focused: bool) -> Block<'static> {
    title.push(Span::raw(" "));
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            theme::border_focused()
        } else {
            theme::border()
        })
        .title(Line::from(title))
}

/// The summary column's width, and the narrowest a pane beside it is worth drawing.
const SUMMARY_WIDTH: u16 = 26;
const PANE_MIN: u16 = 50;
/// A row shorter than this cannot hold a block, a header and a row. It is a floor on every
/// row, the share-the-rest ones and the ones that name their own height alike: a declared
/// `3` would otherwise be accepted here and draw a block with a header and no rows.
const ROW_MIN: u16 = 5;

/// The height a grid row asks for: what it declares, never below the floor. `0` is
/// share-the-rest, which asks for the floor and takes what is left over.
fn row_height(declared: u16) -> u16 {
    declared.max(ROW_MIN)
}

/// How many of a page's grid rows, from the top, a body this tall can hold. The rest are
/// dropped, and the header's `Count:` field says how many panes went with them.
///
/// The arithmetic is saturating because `row_heights` is generator data: a page declaring two
/// enormous rows must drop a row, not panic the draw loop in a debug build.
fn page_rows(def: &'static nutsh_catalog::PageDef, height: u16) -> usize {
    let mut used = 0u16;
    let mut rows = 0;
    for h in def.row_heights {
        let want = row_height(*h);
        if used.saturating_add(want) > height {
            break;
        }
        used = used.saturating_add(want);
        rows += 1;
    }
    rows
}

fn draw_page(app: &App, f: &mut Frame, area: Rect) {
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
fn draw_pane(app: &App, live: &Live, page: &PageView, i: usize, f: &mut Frame, area: Rect) {
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
fn draw_summary(page: &PageView, f: &mut Frame, area: Rect) {
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

/// One line of the contexts list. The header goes through it too, so the columns cannot drift
/// from their titles. `lead` is the three-character selection-and-current marker.
fn context_line(lead: &str, cells: [&str; 6]) -> String {
    let [name, host, user, cluster, flags, password] = cells;
    // Every column but the last is cut to its own width: a long host name must not push the
    // user and the flags out from under their headings.
    format!(
        "{lead}{} {} {} {} {} {password}",
        field(name, 14),
        field(host, 24),
        field(user, 10),
        field(cluster, 14),
        field(flags, 18),
    )
}

/// How many lines of the message area the screen gives the last result.
const MESSAGE_LINES: usize = 3;

/// The contexts, then a message area for the last thing that happened: a connect in flight, an
/// error, a removal, the confirmation of one.
///
/// The two are laid out rather than concatenated. A list longer than the screen would
/// otherwise push the message off the bottom, and `remove NAME? y/n` below the fold is a
/// question the user answers `y` to without ever seeing it.
fn draw_contexts(app: &App, f: &mut Frame, body: Rect) {
    let s = &app.screen;
    let width = usize::from(body.width);
    let said = message_lines(s, width);
    // A blank line above what it says, and never more of the body than the list itself.
    let wanted = if said.is_empty() { 0 } else { said.len() + 1 };
    let height = u16::try_from(wanted)
        .unwrap_or(u16::MAX)
        .min(body.height.saturating_sub(1));
    let [list, message] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).areas(body);
    f.render_widget(Paragraph::new(context_lines(s, list, width)), list);
    let said: Vec<Line> = std::iter::once(Line::from(""))
        .chain(said.into_iter().map(Line::from))
        .collect();
    f.render_widget(Paragraph::new(said), message);
}

/// What the screen has to say under the rows: the removal it is waiting on, or the last
/// result and whatever reading the rows had to say.
fn message_lines(s: &contexts::Screen, width: usize) -> Vec<String> {
    if let Some(name) = &s.confirm_remove {
        return vec![cut(&format!("remove {name}? y/n"), width)];
    }
    [s.message.as_deref(), s.list_error.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(str::lines)
        .take(MESSAGE_LINES)
        .map(|line| cut(line, width))
        .collect()
}

/// The heading and as many rows as `area` holds, framing the selection like the palette and
/// the picker do; the count of what is left below goes on the last line, like the `(N more)`
/// on a modal's border.
fn context_lines(s: &contexts::Screen, area: Rect, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // "You have no contexts" and "your config file could not be read" are different answers,
    // and inviting the user to add a context is wrong advice for the second.
    if s.rows.is_empty() {
        if s.list_error.is_none() {
            lines.push(Line::from(
                "No contexts. Press a to add one, or run nutsh ctx add.",
            ));
        }
        return lines;
    }
    lines.push(
        Line::from(cut(
            &context_line(
                "   ",
                ["NAME", "HOST", "USER", "CLUSTER", "FLAGS", "PASSWORD"],
            ),
            width,
        ))
        .style(Style::default().add_modifier(Modifier::BOLD)),
    );
    // The heading, and the hint line whenever the list is longer than the area - reserved
    // whether or not anything is below the window, so scrolling does not move every row by
    // one when the last of them comes into view.
    let room = usize::from(area.height).saturating_sub(1);
    let scrolls = s.rows.len() > room;
    let rows = if scrolls {
        room.saturating_sub(1)
    } else {
        room
    };
    let (offset, window) = crate::palette::window(&s.rows, s.selected, rows);
    for (i, r) in window.iter().enumerate() {
        // The default port is implied; anything else is part of the address.
        let host = if r.port == DEFAULT_PORT {
            r.host.clone()
        } else {
            format!("{}:{}", r.host, r.port)
        };
        let mut flags = Vec::new();
        if r.insecure {
            flags.push("insecure");
        }
        if r.readonly {
            flags.push("read-only");
        }
        // Not `marker()`: this list carries two markers, the cursor and the `*` of the
        // current context, and they have to sit in fixed columns of their own.
        let lead = format!(
            "{}{} ",
            if offset + i == s.selected { ">" } else { " " },
            if r.current { "*" } else { " " }
        );
        lines.push(Line::from(cut(
            &context_line(
                &lead,
                [
                    &r.name,
                    &host,
                    &r.username,
                    r.cluster.as_deref().unwrap_or("-"),
                    &flags.join(" "),
                    if r.has_password { "stored" } else { "-" },
                ],
            ),
            width,
        )));
    }
    if scrolls {
        let more = s.rows.len() - (offset + window.len());
        lines.push(Line::from(if more > 0 {
            format!("({more} more)")
        } else {
            String::new()
        }));
    }
    lines
}

/// The add form, or the login field, which is the same box with only its password row. Sized
/// to what it holds: a one-field prompt in a box for eight would read as a broken form.
fn draw_form(app: &App, f: &mut Frame, body: Rect) {
    let s = &app.screen;
    /// What the fields are laid out for; a longer line widens the box rather than being cut.
    const WIDTH: u16 = 60;
    // `▸ ` marks the field the keys go to, and the cursor says where the next one lands: a
    // checkbox takes none - space toggles it - so it gets the marker alone.
    let row = |active: bool, cursor: Option<usize>, label: &str, value: String| -> Line<'static> {
        let marker = if active {
            Span::styled("▸ ", Style::default().fg(theme::peach()))
        } else {
            Span::raw("  ")
        };
        let mut spans = vec![marker, Span::styled(format!("{label:<10} "), theme::dim())];
        spans.extend(owned_cursor_spans(
            &value,
            cursor,
            Style::default().fg(theme::text()),
            theme::peach(),
        ));
        Line::from(spans)
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    let (title, error) = match &s.prompt {
        Some(Prompt::Login(login)) => {
            lines.push(row(
                true,
                Some(login.cursor()),
                "password",
                contexts::mask(&login.password),
            ));
            (
                format!("password for {}", login.name),
                login.error.as_deref(),
            )
        }
        Some(Prompt::Add(form)) => {
            for field in contexts::FIELDS {
                let active = field == form.current();
                // `Form::cursor` answers for the field the keys go to, and `None` for a
                // checkbox - the same split as `Form::text_mut`.
                let cursor = active.then(|| form.cursor()).flatten();
                lines.push(row(active, cursor, field.label(), form.value(field)));
            }
            ("add context".to_string(), form.error.as_deref())
        }
        None => return,
    };
    // While the connect runs the box stays open with everything typed in it, so it says what
    // it is waiting for where the answer will go: the pending message dim, an error red.
    let note = if s.pending {
        s.message
            .as_deref()
            .map(|m| Span::styled(m.to_string(), theme::dim()))
    } else {
        error.map(|e| Span::styled(e.to_string(), Style::default().fg(theme::red())))
    };
    if let Some(note) = note {
        lines.push(Line::from(""));
        lines.push(Line::from(note));
    }
    boxed_lines(f, body, &title, lines, WIDTH);
}

/// A centred, rounded box around `lines`: wide enough for the widest of them and never
/// narrower than `min_width`, because a sentence cut in half explains nothing. A body narrower
/// or shorter than that clamps the box, and the paragraph clips at the border.
///
/// The Contexts prompt and an action's form are the same box around different rows; the rows
/// are what differs between them, and this is not.
fn boxed_lines(f: &mut Frame, body: Rect, title: &str, lines: Vec<Line<'static>>, min_width: u16) {
    let widest = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = u16::try_from(widest.saturating_add(2))
        .unwrap_or(u16::MAX)
        .max(min_width);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered_rect_with_min(0, 0, width, height, body);
    clear_region(f, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::peach()))
                .title(Span::styled(format!(" {title} "), theme::title())),
        ),
        area,
    );
}

/// The chosen action's form: one row per visible field, the cursor on the selected one, and
/// what `build_body` refused with under a blank line. The reference picker, when one is open,
/// is drawn after it - it is a list of rows, and the form is what it writes into.
///
/// Not [`draw_form`], which draws the Contexts screen's fixed struct: these rows come from the
/// catalog, and their types decide how a value is shown.
fn draw_fields(app: &App, f: &mut Frame, body: Rect) {
    let Some(form) = &app.form else {
        return;
    };
    /// What the rows are laid out for; a longer line widens the box rather than being cut.
    const WIDTH: u16 = 60;
    /// The label column. Wide enough for the longest curated label, and a longer one pushes
    /// its own value right rather than being cut.
    const LABEL: usize = 16;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, field) in form.fields.iter().enumerate() {
        let active = i == form.selected;
        let raw = form.value(field.name);
        // A checkbox shows its state rather than the word `true`, and takes no cursor: space
        // toggles it. Everything else is typed, so the cursor says where the next key lands.
        let (value, cursor) = match field.ty {
            FieldType::Bool => {
                let box_ = if raw == "true" { "[x]" } else { "[ ]" };
                (box_.to_string(), None)
            }
            FieldType::Enum(_) => (raw.to_string(), None),
            _ => (raw.to_string(), active.then(|| form.cursor())),
        };
        // A required field says so where it is read, not in a legend at the bottom.
        let name = nutsh_core::actions::label_of(field);
        let label = if field.required {
            format!("{name} *")
        } else {
            name.to_string()
        };
        let mut spans = vec![
            if active {
                Span::styled("▸ ", Style::default().fg(theme::peach()))
            } else {
                Span::raw("  ")
            },
            Span::styled(format!("{label:<LABEL$} "), theme::dim()),
        ];
        spans.extend(owned_cursor_spans(
            &value,
            cursor,
            Style::default().fg(theme::text()),
            theme::peach(),
        ));
        lines.push(Line::from(spans));
    }
    // `create` on a VM has thirty-eight visible fields and twenty catalog forms have fifteen
    // or more, while the box is clamped to the body: unwindowed, `tab` walks the cursor onto
    // rows that were never drawn, and the refusal underneath goes off the bottom with them.
    // The error keeps its two lines whatever is showing, so it is always the last thing in the
    // box rather than line n+2 of a column that no longer reaches n.
    let error = form.error.as_deref().map(|e| {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                e.to_string(),
                Style::default().fg(theme::red()),
            )),
        ]
    });
    let reserved = 2 + error.as_ref().map_or(0, Vec::len);
    let rows = usize::from(body.height).saturating_sub(reserved);
    let (_, shown) = crate::palette::window(&lines, form.selected, rows);
    let mut lines = shown.to_vec();
    lines.extend(error.unwrap_or_default());
    boxed_lines(f, body, &form.title, lines, WIDTH);
    draw_ref_picker(form, f, body);
}

/// The rows of the kind a `Reference` field names, beside the form it writes into. Empty while
/// the one-shot list is still in flight, which is what `loading…` says.
fn draw_ref_picker(form: &crate::form::Form, f: &mut Frame, body: Rect) {
    let Some(p) = form.picker.as_ref() else {
        return;
    };
    // The corner, not the centre: the form is centred, and a box over the fields would hide
    // the very rows the choice is being made for.
    let area = popup_area(body, p.entries.len().max(1));
    clear_region(f, area);
    let rows = usize::from(area.height.saturating_sub(2));
    let (offset, window) = p.window(rows);
    let width = usize::from(area.width);
    let mut items: Vec<ListItem> = window
        .iter()
        .map(|choice| {
            tagged_row(
                &choice.name,
                Style::default().fg(theme::text()),
                Span::styled(elide_left(&choice.ext_id, 16), theme::dim()),
                width,
            )
        })
        .collect();
    if items.is_empty() {
        // Three states, not two: still walking the pages, done and empty, or done and failed.
        // A kind that could not be read saying "no rows" is a lie the user would act on.
        let note = match (form.loading, form.listing_error.as_ref()) {
            (true, _) => "loading…".to_string(),
            (false, Some(e)) => format!("list failed: {}", e.text()),
            (false, None) => "no rows".to_string(),
        };
        items.push(ListItem::new(Span::styled(note, theme::dim())));
    }
    // The placeholder is not a row `enter` could take, so nothing is selected on it.
    let selected = (!p.entries.is_empty()).then(|| p.selected - offset);
    let mut state = ListState::default().with_selected(selected);
    render_framed_list(
        f,
        area,
        items,
        Line::from(vec![
            Span::styled(" choose ", theme::title()),
            Span::styled(
                format!("{} ", cut(p.label, 28)),
                Style::default().fg(theme::peach()),
            ),
        ]),
        Scroll::of(offset, window.len(), p.entries.len()),
        &mut state,
    );
}

/// The popup's geometry: bottom-left of the body, one cell in, 46 wide, at most 12 rows.
const POPUP_WIDTH: u16 = 46;
/// The narrowest a confirm dialog is drawn: `Delete 7 Virtual Machines. Type DELETE 7 to
/// confirm:` is fifty-one cells, and a prompt cut in half is one nobody can answer.
const CONFIRM_WIDTH: u16 = 54;
const POPUP_ROWS: u16 = 12;

fn popup_area(body: Rect, rows: usize) -> Rect {
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
fn share(label: &str, tag: &str, inner: usize) -> (usize, usize) {
    let label_floor = label.width().min(inner / 2);
    let tag_cells = tag.width().min(inner.saturating_sub(label_floor + 1));
    (inner.saturating_sub(tag_cells + 1), tag_cells)
}

/// `s` in at most `room` cells, cut from the right with a marker where it was cut: the head
/// of a reason is the half that says what is wrong.
fn elide_right(s: &str, room: usize) -> String {
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
fn tagged_row<'a>(label: &str, style: Style, tag: Span<'a>, width: usize) -> ListItem<'a> {
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
fn greyable_row<'a>(
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

/// The suggestions under the `:` prompt: as many ranked entries as fit around the selection,
/// each tagged with what it is - `cmd`, the kind's id, or the reason it is greyed.
fn draw_palette(app: &App, f: &mut Frame, body: Rect) {
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

/// The child kinds of the row `enter` was pressed on, greyed where the row cannot open them.
fn draw_picker(app: &App, f: &mut Frame, body: Rect) {
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

/// A box sized to what it holds, centred in the body: `rows` rows plus its border, never
/// taller than the body, and `percent` of its width but never under `min_width`.
///
/// Not [`popup_area`], which anchors to the bottom-left corner and caps at twelve rows: an
/// action menu is the length the kind's action list is, and it is read against the whole body
/// rather than beside the `:` prompt it does not have.
fn dialog_area(body: Rect, rows: usize, percent: u16, min_width: u16) -> Rect {
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
const DIALOG_PERCENT: u16 = 70;

/// The actions of the row kind's `action_target`, greyed where this session, this account or a
/// local rule refuses them. Three columns, like the palette and the picker: the title, and
/// either the key that runs it or the reason it cannot run.
fn draw_menu(app: &App, f: &mut Frame, body: Rect) {
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

/// `[P]` for an action a key runs, nothing for one only the menu reaches.
fn key_hint(action: &'static nutsh_catalog::Action) -> String {
    if action.key.is_empty() {
        String::new()
    } else {
        format!("[{}]", action.key)
    }
}

/// The prompt, the rows a bulk action would touch, and - for a type-name confirm - what has
/// been typed towards the phrase it wants.
fn draw_confirm(app: &App, f: &mut Frame, body: Rect) {
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
fn draw_skins(app: &App, f: &mut Frame, body: Rect) {
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

/// Three columns: what it is, what it is now, and where that came from. The chrome is
/// `draw_skins`', because a second popper-upper is a second thing to keep looking the same.
fn draw_settings(app: &App, f: &mut Frame, body: Rect) {
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

fn cut(s: &str, width: usize) -> String {
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
fn field(s: &str, width: usize) -> String {
    pad(cut(s, width), width)
}

/// `s` in at least `width` display cells. For the column before the one that may be lost: the
/// reason a palette entry is greyed has to survive a narrow terminal, and pushing the id along
/// - which `cut` then takes off the end - is how it does.
fn pad(mut s: String, width: usize) -> String {
    s.extend(std::iter::repeat_n(' ', width.saturating_sub(s.width())));
    s
}

fn draw_detail(app: &App, f: &mut Frame, body: Rect) {
    let Some(d) = &app.detail else {
        return;
    };
    // `None` on a payload pane, which is over no row at all, and on an entity pane whose row
    // has not landed yet; the pane words the two differently.
    let entity = app.detail_entity();
    // A payload pane reads nothing from the store, so it must not need a session to be drawn:
    // without one the cache is simply empty, and every reference renders as its own extId.
    let empty = Names::default();
    let names = app.live.as_ref().map_or(&empty, |l| l.store.names());
    // Recorded for `App::detail_last_line`, which measures the same lines and cannot reach the
    // layout: the pane's interior is its width less the two border columns.
    let width = body.width.saturating_sub(2);
    app.detail_width.set(width);
    // The one list every action surface draws, measured by `App::detail_last_line` off the same
    // call: a pane whose last line the scroll does not know about is one that cannot be read to
    // the end.
    let actions = app.detail_actions();
    let lines = d.lines(crate::detail::BodyView {
        entity,
        names,
        now: app.now,
        width,
        actions: &actions,
    });
    // Scrolling past the end would leave an empty box with no way to tell why.
    let scroll = d.scroll.min(Detail::last_line(&lines));
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(d.title(entity)))
            .scroll((scroll, 0)),
        body,
    );
}

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
const JOURNAL_WIDTHS: [Constraint; 7] = [
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
fn draw_journal(app: &App, f: &mut Frame, body: Rect) {
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
fn kind_label(kind: &'static nutsh_catalog::Kind) -> &'static str {
    kind.aliases.first().copied().unwrap_or(kind.id)
}

/// The rows the reach line and the box's two borders take off the results list.
const SEARCH_CHROME: u16 = 3;

/// `:search`, over the whole body: what every loaded kind holds under one term, grouped by
/// kind, with the reach under it.
///
/// The reach line is drawn last and is never scrolled away. It is the sentence that makes the
/// list readable: a search of two kinds out of two hundred and sixty-two looks exactly like a
/// search of all of them until something says which it was, and a caption that scrolled off
/// with the results would say it only to whoever started at the top.
fn draw_search(app: &App, f: &mut Frame, body: Rect) {
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
fn search_row<'a>(row: &'a crate::search::Row, width: u16) -> ListItem<'a> {
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

/// The box the `?` overlay is drawn in, borders included, and the one number this file will
/// not negotiate: `help_draws_over_the_view_it_was_opened_from` renders at 100×27, where the
/// body is 18 rows, and a box taller than that clamps and covers the title the test asserts
/// on. So the interior is `HELP_BOX.1 - 2` rows by `HELP_BOX.0 - 2` cells, `HELP` fills both
/// exactly, and `the_help_text_fits_its_box` measures it - `Paragraph` clips rather than
/// wraps and reports nothing when it does.
const HELP_BOX: (u16, u16) = (70, 16);

/// The keys, in fourteen lines of sixty-eight cells.
///
/// The mouse rows are here and nowhere else: no prompt line advertises a gesture, so over SSH
/// or in a terminal with reporting off the app reads identically - and "why can't I copy the
/// error message" is the first bug report that feature will produce, which is why `^o` and
/// shift-drag are named here rather than left to be found.
///
/// `a` and `space` are here for the opposite reason: they were named nowhere, and twenty-five
/// VM actions sat behind them. Room for two lines came from folding `esc back / close` onto
/// the `enter` row and `ctrl-r refresh now` onto the `?` row - the inline second key that rows
/// 4, 6 and 14 already use - rather than from a fifteenth line, which would have vanished.
///
/// `/` cost `w` its own line, which is now the second key on the `S` row: the two of them
/// reshape the table between them, and `w wide` is also in the header's hint grid where `/`
/// was in neither. Same idiom again, and still fourteen lines.
///
/// `:search` then cost nothing: it shares the `/` row, because the two of them are one gesture
/// at two reaches and the sentence that distinguishes them is the same sentence - these rows,
/// or every loaded kind. What it did cost is `esc clears it`, which `esc back / close` two rows
/// up already half says.
///
/// The command list on row 2 is a selection, not an index: it names the commands nothing else
/// on the overlay names. So `search` left it when the `/` row arrived and `help` left it when
/// `?` was spelled out at the bottom, and `settings` took the room that freed.
///
/// `^x` cost nothing either, and the room came from the box spelling one modifier two ways:
/// `^w` on row 3 against `ctrl-o`, `ctrl-r` and `ctrl-c` at the bottom. One spelling throughout,
/// the same `^` the header's hint grid uses, pays for `^x stop the walk` on the last row
/// with cells to spare, and reads as one vocabulary instead of two. `^t schedule` then fitted
/// beside them, and the last row lost only the words `now` and `the walk`.
///
/// `^p ^n history` cost `⏎ run` its place on row 3. Of the four keys on that row it is the one
/// a person guesses without being told - `enter` runs the thing under the cursor in every list
/// this program draws - where `^w` and `^p`/`^n` are named here or nowhere.
const HELP: &str = "\
:            palette: a kind, a page, or a command
             (ctx, skin, mouse, settings, refresh, journal, can-i)
             ⇥ complete · ↑↓ pick · ^p ^n history · ^w delete word
j k g G      move        PgUp PgDn   page
enter        drill into children, or detail     esc   back / close
y            detail (composed)   Y  raw YAML    J  raw JSON
a            actions for this row; the menu shows each one's key
space        mark a row; a then acts on every marked row
S            sort by next column, then reverse   w  wide: 20 columns
/  :search   match name, IP or id here, or across every loaded kind
mouse        click a row, a menu item or a header; the wheel scrolls
             the pane under it; shift-drag still selects text
^o           mouse capture off for this session; :mouse, for good
?            this help  ^r refresh  ^x stop  ^t schedule  ^c quit";

fn draw_help(f: &mut Frame, body: Rect) {
    let area = centered_rect_with_min(0, 0, HELP_BOX.0, HELP_BOX.1, body);
    clear_region(f, area);
    f.render_widget(
        Paragraph::new(HELP).block(Block::bordered().title("keys")),
        area,
    );
}

// The three modal primitives below are from sofka `src/ui.rs:3524-3606` (MIT OR Apache-2.0,
// Copyright (c) 2026 Nikola Milojević).

/// Clear a popup region before drawing on top of it. `Clear` resets the cells to the terminal
/// default; with the skin background enabled that would punch a transparent hole through the
/// fill, so repaint `base` over the cleared cells.
fn clear_region(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    if let Some(bg) = theme::background() {
        f.buffer_mut().set_style(area, Style::default().bg(bg));
    }
}

/// The one list widget every modal uses: a rounded focused border, the `▌ ` gutter always
/// reserved, and the selection in `selected_row()`.
fn render_framed_list<'a>(
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
struct Scroll {
    above: usize,
    below: usize,
    total: usize,
}

impl Scroll {
    /// The window a `palette::window` call returned: `offset` skipped, `shown` drawn, `total`
    /// in the list.
    fn of(offset: usize, shown: usize, total: usize) -> Self {
        Self {
            above: offset,
            below: total.saturating_sub(offset + shown),
            total,
        }
    }

    fn scrolls(self) -> bool {
        self.above > 0 || self.below > 0
    }

    fn shown(self) -> usize {
        self.total.saturating_sub(self.above + self.below)
    }
}

/// A rectangle that is `percent` of `r` but never smaller than `min`, centred and clamped to
/// `r`. Used by popups that size themselves to their content rather than to a percentage.
fn centered_rect_with_min(
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

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use ratatui::layout::Rect;

    use ratatui::buffer::Buffer;
    use ratatui::text::Span;
    use ratatui::widgets::{List, ListItem, Widget};

    use super::{
        HELP, HELP_BOX, centered_rect_with_min, cut, elide_left, elide_right, field, greyable_row,
        pad, popup_area, share,
    };

    /// The source-level half of the overlay's fit. `the_help_overlay_fills_its_box_and_no_more`
    /// measures the drawn frame; this one says which line is at fault before a frame is drawn,
    /// because `Paragraph` clips in silence and the box cannot grow - 18 rows is the body at
    /// 100×27, and a taller box covers the title of the view the help was opened over.
    #[test]
    fn the_help_text_fits_its_box() {
        let (cols, rows) = (usize::from(HELP_BOX.0) - 2, usize::from(HELP_BOX.1) - 2);
        assert_eq!(HELP.lines().count(), rows, "the interior is {rows} rows");
        for line in HELP.lines() {
            assert!(
                line.width() <= cols,
                "{} cells, and the interior is {cols}: {line}",
                line.width()
            );
        }
    }

    /// The confirm rectangle the administration spec builds on: `centered_rect_with_min(50,
    /// 20, 56, 7, body)`, centred and never smaller than its minimum. The chrome drawn in it
    /// (cleared, rounded red border, red title, question in `text`, hints yellow) is that
    /// spec's to test with its first dialog.
    #[test]
    fn the_confirm_rect_is_at_least_fifty_six_by_seven() {
        let body = Rect::new(0, 0, 100, 24);
        let area = centered_rect_with_min(50, 20, 56, 7, body);
        assert_eq!(
            (area.width, area.height),
            (56, 7),
            "the minimum wins at 100x24"
        );
        assert_eq!((area.x, area.y), (22, 8), "centred");
        let wide = centered_rect_with_min(50, 20, 56, 7, Rect::new(0, 0, 200, 60));
        assert_eq!(
            (wide.width, wide.height),
            (100, 12),
            "the percentage wins when it is bigger"
        );
        let tiny = centered_rect_with_min(50, 20, 56, 7, Rect::new(0, 0, 20, 4));
        assert_eq!((tiny.width, tiny.height), (20, 4), "clamped to the body");
    }

    /// The popup stays inside the body it is drawn over: one cell in from the left, one row
    /// above the bottom border, and never past the right one on a body narrower than it.
    #[test]
    fn the_popup_stays_inside_a_narrow_body() {
        let narrow = popup_area(Rect::new(0, 0, 40, 18), 5);
        assert_eq!((narrow.x, narrow.width), (1, 38));
        assert!(narrow.right() <= 40);
        assert_eq!(narrow.height, 7, "five rows and the border");
        let wide = popup_area(Rect::new(0, 0, 100, 24), 30);
        assert_eq!((wide.width, wide.height), (46, 14), "capped at twelve rows");
        assert_eq!(wide.bottom(), 23, "one row above the body's bottom border");
    }

    /// A tag never pushes its label out of the row: the label keeps what it needs up to half
    /// the row, and the tag is cut from the right - its head is the half that says what is
    /// wrong - into what is left. A greyed row whose name is gone says nothing about what is
    /// greyed.
    #[test]
    fn a_long_reason_leaves_the_label_at_least_half_the_row() {
        // A 46-cell sample, kept for its width: the app no longer emits this sentence - a kind
        // newer than its namespace's pin is attempted now - and every expectation below is
        // arithmetic over those 46 cells.
        let reason = "needs vmm v4.3, this Prism Central serves v4.1";
        assert_eq!(reason.width(), 46, "longer than the whole popup");
        // POPUP_WIDTH less the border and the gutter.
        let inner = 42;
        assert_eq!(
            share("VM Profiles", reason, inner),
            (11, 30),
            "a short label keeps all of itself"
        );
        assert_eq!(elide_right(reason, 30), "needs vmm v4.3, this Prism Ce…");
        assert_eq!(
            share("VM Anti Affinity Policies", reason, inner),
            (21, 20),
            "a long label keeps half the row"
        );
        assert_eq!(
            share("Virtual Machines", "vmm.ahv…Vm", inner),
            (31, 10),
            "a tag that fits changes nothing"
        );
        assert_eq!(elide_right("abc", 3), "abc");
        assert_eq!(elide_right("abcd", 1), "…");
        assert_eq!(elide_right("abcd", 0), "");
        assert_eq!(
            elide_right("東京都庁舎", 5),
            "東京…",
            "cells, not characters"
        );
    }

    /// And the row the palette actually draws: a reason longer than its share of the row is
    /// cut from the right and marked, with the label still whole. Asserted here rather than
    /// through the app, because a long reason can be handed in directly.
    #[test]
    fn a_greyed_row_cuts_the_reason_not_the_name() {
        let reason = "open from Some Kind With A Very Long Display Name";
        let row = render_row(
            greyable_row("Disks", Span::raw("nav"), Some(reason), 46),
            46,
        );
        assert!(row.contains("Disks"), "the label survives: {row:?}");
        assert!(row.contains("open from Some"), "the head of it: {row:?}");
        assert!(row.contains('…'), "and it is marked cut: {row:?}");
        assert!(!row.contains("Display Name"), "cut from the right: {row:?}");
        // A reason that fits is left alone.
        let short = render_row(
            greyable_row(
                "Volume Groups",
                Span::raw("nav"),
                Some("namespace not served"),
                46,
            ),
            46,
        );
        assert!(short.contains("namespace not served"), "{short:?}");
        assert!(!short.contains('…'), "{short:?}");
    }

    /// One `ListItem` drawn into a one-row buffer, as text.
    fn render_row(item: ListItem<'_>, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        Widget::render(List::new(vec![item]), area, &mut buf);
        (0..width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    /// The end of a path is what identifies it, so the front is what goes; the marker says
    /// that something did.
    #[test]
    fn a_path_too_long_for_the_header_keeps_its_tail() {
        let path = "/home/u/.config/nutsh/config.toml";
        assert_eq!(elide_left(path, 40), path, "it fits, so nothing changes");
        assert_eq!(elide_left(path, path.len()), path, "exactly fits");
        assert_eq!(elide_left(path, 19), "…/nutsh/config.toml");
        assert_eq!(elide_left(path, 19).chars().count(), 19);
        assert_eq!(elide_left(path, 1), "…");
        assert_eq!(elide_left(path, 0), "");
    }

    /// Counted in characters, not bytes: a path with a multi-byte segment must not be cut
    /// through the middle of one.
    #[test]
    fn eliding_counts_characters() {
        let path = "/home/ünïcøde/config.toml";
        let out = elide_left(path, 15);
        assert_eq!(out.chars().count(), 15, "{out}");
        assert!(out.ends_with("config.toml"), "{out}");
    }

    /// Every measurement in a frame is in the cells the terminal draws, not the characters
    /// behind them: a CJK name is two cells per character, and a line counted in characters
    /// runs twice as far as the box it was drawn in.
    #[test]
    fn cutting_and_padding_count_display_cells() {
        assert_eq!(cut("東京都庁舎", 6), "東京都");
        assert_eq!(cut("東京", 3), "東", "half a character does not fit");
        assert_eq!(cut("abc", 10), "abc");
        assert_eq!(cut("", 4), "");
        assert_eq!(
            field("東京", 6),
            "東京  ",
            "padded to six cells, not six chars"
        );
        assert_eq!(field("東京都庁舎", 4), "東京");
        assert_eq!(
            field("web-01", 4),
            "web-",
            "and one long value is cut, not shifted"
        );
        assert_eq!(pad("東京".to_string(), 6).width(), 6);
        assert_eq!(pad("東京都庁舎".to_string(), 4), "東京都庁舎", "never cut");
    }

    /// The same for the path in the header: a wide character that only half fits would push
    /// the line one column past the edge.
    #[test]
    fn eliding_counts_display_cells() {
        let path = "/opt/東京/config.toml";
        assert_eq!(elide_left(path, 12).width(), 12);
        assert!(elide_left(path, 12).ends_with("config.toml"));
        assert_eq!(elide_left("/opt/東京都", 7), "…東京都");
        // Six cells cannot hold the marker and three wide characters, and a character that
        // only half fits is left out rather than drawn over the edge.
        assert_eq!(elide_left("/opt/東京都", 6), "…京都");
    }
}
