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

mod contexts_screen;
mod detail;
mod form;
mod header;
mod help;
mod journal;
mod modals;
mod page;
mod palette;
mod prompt;
mod rows;
mod search;
mod settings;
mod sidebar;
mod widgets;

use contexts_screen::*;
use detail::*;
use form::*;
use header::*;
use help::*;
use journal::*;
use modals::*;
use page::*;
use palette::*;
use prompt::*;
use rows::*;
use search::*;
use settings::*;
use sidebar::*;
use widgets::*;
// `crate::ui::rule` is a path the detail pane takes; the glob does not re-export.
pub(crate) use widgets::rule;

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

/// The sync indicator's cell. `◌ cached 12m ` is thirteen columns - the widest of the three
/// states - with three spare for an age that runs to `100d`.
const SYNC_WIDTH: u16 = 16;

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

// The three modal primitives below are from sofka `src/ui.rs:3524-3606` (MIT OR Apache-2.0,
// Copyright (c) 2026 Nikola Milojević).

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
