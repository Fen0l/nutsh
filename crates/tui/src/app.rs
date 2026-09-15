//! The state machine. `handle` takes keys, `apply` takes poll messages, `draw` renders; none
//! of them touch the terminal.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use nutsh_catalog::{Action, Column, Kind, Method, PageDef, Reach, lookup, reach};
use nutsh_core::actions::{Outcome, Plan, PlanRef, Policy};
use nutsh_core::can_i::{CanI, CanICell};
use nutsh_core::contexts::{ConnectRequest, Connected, Contexts};
// Re-exported through `app`, where `Config` names it: a caller that builds a `Config` - the
// binary, and every frame test - should not have to reach past the TUI for one of its fields.
pub use nutsh_core::contexts::Header;
use nutsh_core::guardrails::{Guardrails, Verdict};
use nutsh_core::journal::{Attempt, Journal, JournalId, JournalOutcome};
use nutsh_core::scheduler::{Msg, Scheduler, SubId, Subscription};
use nutsh_core::session::Session;
use nutsh_core::store::{Store, TableKey};
use nutsh_core::tasks::{TaskIndex, TaskStatus};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::confirm::{self, Confirm};
use crate::contexts::{self as screen, Screen};
use crate::detail::{self, Detail};
use crate::form::{self, Form, RefPicker};
use crate::journal::{self, JournalView};
use crate::key::Key;
use crate::menu::{self, Menu};
use crate::mouse::Mouse;
use crate::page::PageView;
use crate::palette::{self, Command, Entry, Palette};
use crate::picker::{self, Picker};
use crate::skins::{self, Skins};

mod actions;
mod detail_screen;
mod form_screen;
mod journal_screen;
mod mouse;
mod navigation;
mod palette_screen;
mod search;
mod settings;

mod messages;
mod session;

#[cfg(feature = "testing")]
mod testing;

/// The narrowest frame the menu is drawn beside a table on by default. Under it a split would
/// leave a 66-cell body, which is not a table, so the menu waits to be asked for with `ctrl-b`
/// and is then drawn over the body rather than beside it.
pub(crate) const MENU_SPLIT_COLUMNS: u16 = 90;

/// The width `App` assumes until a frame has been drawn: the plan's reference frame. Every
/// draw path goes through `ui::draw`, which records the real one, so this is only what the
/// keys before the first frame see.
const REFERENCE_WIDTH: u16 = 100;

/// How long a change waits before it reaches the disk. The write runs off the tick the event
/// loop already has, so the debounce is one subtraction rather than a timer of its own.
const CACHE_PERIOD: u64 = 60;

/// What the binary decides at startup and tests pin.
#[derive(Debug, Clone)]
pub struct Config {
    /// The clock used for ages; the event loop advances it every tick.
    pub now: SystemTime,
    /// Where the contexts come from, shown in the header of the Contexts screen; `None` in
    /// tests that construct an app without a config file.
    pub config_path: Option<String>,
    /// The local rules that can refuse a mutation: `--readonly`, the context's, the file's
    /// `[[guardrails]]`, and the built-ins under them.
    pub guardrails: Guardrails,
    /// This run renders one frame and exits (`--snapshot`). The meter is a timing value and is
    /// left out of it; everything else on the frame is the same. Not a renderer flag: the
    /// TUI's own frame tests draw through `App::snapshot` too, and they *want* the meter, with
    /// an `insta` filter over it.
    pub snapshot: bool,
    /// Whether the event loop captures the mouse; `false` is `mouse = false` in the config file.
    pub mouse: bool,
    /// How tall the header is drawn: `header` in the config file, `auto` unless it says
    /// otherwise.
    pub header: Header,
}

/// Which pane the arrow keys move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Contexts,
    Form,
    Table,
    Command,
    Picker,
    Detail,
    Help,
    Skins,
    /// The action menu `a` opens over the table.
    Menu,
    /// The confirm dialog between a chosen action and the request.
    Confirm,
    /// The generic form built from a chosen action's `Field`s, and the reference picker that
    /// opens over one of them. `Mode::Form` is the Contexts screen's own fixed form and has
    /// nothing to do with this one.
    Fields,
    /// `:journal`: what was attempted this session, over whatever it was opened from.
    Journal,
    /// `:search`: what every kind already loaded holds under one term.
    Search,
    /// `:settings`: what is set, what it is set to, and who set it.
    Settings,
}

pub struct TableView {
    pub key: TableKey,
    pub sub: SubId,
    pub selected: usize,
    pub sort: Option<(usize, bool)>,
    pub wide: bool,
    /// How many columns after the first are scrolled off the left edge.
    pub col_offset: usize,
    /// The parent row's name, for the breadcrumb.
    pub parent_name: Option<String>,
    /// The extIds `space` has marked: what an action runs on instead of the row under the
    /// cursor. Per view, so drilling into a child and coming back finds them where they were.
    pub marks: std::collections::HashSet<String>,
    /// What `/` has narrowed the view to, while it is narrowed. Per view and not per kind, so
    /// it lives exactly as long as the view does: drilling into a child and coming back finds
    /// it where it was, the way the marks beside it do, and opening the same kind fresh from
    /// the menu or the palette - which drains the stack - opens it whole.
    ///
    /// Boxed because almost no view has one: a `Filter` carries a line of text and its parse,
    /// and inlining that in every `TableView` makes `View::Table` four times the size of
    /// `View::Page` for a field that is `None` on all but the view being looked at.
    pub(crate) filter: Option<Box<crate::table::Filter>>,
    /// `ctrl-x` stopped this view's walk. Per view and per session, cleared by `ctrl-r`, a
    /// re-open or a context switch: whether a stopped kind should stay stopped across a
    /// re-open is a `[nav]`-shaped question this phase does not answer.
    pub stopped: bool,
}

impl TableView {
    /// What the rows are matched against, or `None` when the view is showing all of them.
    pub(crate) fn query(&self) -> Option<&nutsh_core::search::Query> {
        self.filter.as_ref()?.query()
    }
}

/// What the body is showing. A page is a view like a table: it can be drilled out of and
/// popped back to, and the stack is what makes `esc` mean one thing everywhere.
pub enum View {
    Table(TableView),
    Page(PageView),
}

impl View {
    fn release(&mut self, scheduler: &mut Scheduler, store: &mut Store) {
        match self {
            View::Table(_) => {}
            View::Page(page) => release_page(page, scheduler, store),
        }
    }

    fn reacquire(&mut self, scheduler: &mut Scheduler) {
        match self {
            View::Table(_) => {}
            View::Page(page) => page.reacquire(scheduler),
        }
    }
}

/// Everything that exists only while connected.
pub struct Live {
    pub session: Session,
    pub store: Store,
    pub scheduler: Scheduler,
    pub stack: Vec<View>,
    /// The stats poller. Only `Scheduler` aborts its tasks on drop; a bare `tokio::spawn`
    /// would outlive the session and drop the previous Prism Central's counters into the new
    /// store. Crate-internal for the reason `PageView::sampler` is: a caller outside could
    /// take the handle and leak the task, which nothing but this type's own `Drop` would then
    /// stop.
    pub(crate) stats: tokio::task::JoinHandle<()>,
    /// The name-cache warm-up, on the same terms as `stats`: only `Scheduler` aborts its tasks
    /// on drop, so a bare `tokio::spawn` would outlive the session and drop the previous Prism
    /// Central's names into the new store.
    pub(crate) names: tokio::task::JoinHandle<()>,
    /// What was attempted this session, refusals included; never persisted. Crate-internal
    /// beside `stats` and for the same reason: `:journal` and the task line are in this crate,
    /// and a caller outside could settle an entry the app is still watching.
    pub(crate) journal: Journal,
    /// The Prism Central tasks this session started and is still following.
    pub(crate) tasks: TaskIndex,
    /// Whether this account may act, resolved lazily in the background.
    pub(crate) can_i: CanICell,
    /// `q` was pressed once with a watch still running; the next one quits.
    pub(crate) quit_asked: bool,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stats.abort();
        self.names.abort();
    }
}

impl Live {
    /// The top view when it is a table; `None` on a page, which is what every table key wants.
    pub fn table(&self) -> Option<&TableView> {
        match self.stack.last()? {
            View::Table(t) => Some(t),
            View::Page(_) => None,
        }
    }

    pub fn table_mut(&mut self) -> Option<&mut TableView> {
        match self.stack.last_mut()? {
            View::Table(t) => Some(t),
            View::Page(_) => None,
        }
    }

    pub fn page(&self) -> Option<&PageView> {
        match self.stack.last()? {
            View::Page(p) => Some(p),
            View::Table(_) => None,
        }
    }

    /// Crate-internal: a caller outside would move a pane's cursor without the clamp that
    /// every rows-changed path goes through.
    pub(crate) fn page_mut(&mut self) -> Option<&mut PageView> {
        match self.stack.last_mut()? {
            View::Page(p) => Some(p),
            View::Table(_) => None,
        }
    }

    /// After the rows change under it, no selection can point past the end. A poll that
    /// returns a shorter list is the ordinary way this happens; a page clamps every pane,
    /// since one message moves one pane's rows and the others keep theirs.
    fn clamp_selection(&mut self) {
        let Live { store, stack, .. } = self;
        match stack.last_mut() {
            Some(View::Table(view)) => {
                // The filtered length, not the table's: the selection indexes what the frame
                // shows, and a poll that dropped a matching row must not leave the cursor past
                // the end of what is left.
                let len = crate::table::matching(store.table(&view.key), view.query());
                view.selected = view.selected.min(len.saturating_sub(1));
            }
            Some(View::Page(page)) => {
                for pane in &mut page.panes {
                    let len = store.table(&pane.key).rows.len();
                    pane.selected = pane.selected.min(len.saturating_sub(1));
                }
            }
            None => {}
        }
    }

    /// The open page's summary box, recomputed from the store: nine lines of arithmetic over
    /// counters already in memory, so it is rebuilt whenever a pane's rows move rather than
    /// cached against them.
    fn refresh_summary(&mut self) {
        let Live { store, stack, .. } = self;
        if let Some(View::Page(page)) = stack.last_mut()
            && let Some(id) = page.def.summary
        {
            page.summary = crate::page::summary::build(id, store);
        }
    }
}

pub struct App {
    pub live: Option<Live>,
    pub mode: Mode,
    /// What the last action wants the screen to say; cleared by the next one.
    pub status: Option<String>,
    /// Set by anything that changes what a frame would look like; the event loop redraws and
    /// clears it.
    pub dirty: bool,
    pub now: SystemTime,
    /// The `:` palette, while it is open.
    pub palette: Option<Palette>,
    /// The child-kind picker `enter` opens over a row, while it is open.
    pub picker: Option<Picker>,
    /// The entity pane, while it is open; it owns a subscription of its own.
    pub detail: Option<Detail>,
    /// The `:skin` list, while it is open.
    pub skins: Option<Skins>,
    /// The settings screen, while it is open.
    pub settings: Option<crate::settings::Settings>,
    /// The action menu, while it is open.
    pub menu: Option<Menu>,
    /// The confirm dialog, while it is open.
    pub confirm: Option<Confirm>,
    /// The chosen action's form, while it is open; it owns the reference picker opened over
    /// one of its fields. Not the Contexts screen's form, which lives in `screen`.
    pub form: Option<Form>,
    /// The `:journal` list, while it is open; the entries themselves live in `Live`, so what
    /// this holds is a cursor over them.
    pub journal_view: Option<JournalView>,
    /// The `:search` results, while they are open. The rows are a snapshot of the store as it
    /// was when the term was run: a poll that lands underneath does not reshuffle a list the
    /// user is reading, and `⏎` opens the row by its identifier rather than by its index.
    pub(crate) search: Option<crate::search::Search>,
    /// The action chosen but not yet sent: the one place the menu, the form and the confirm
    /// hand work to each other.
    pub(crate) pending: Option<Pending>,
    /// The local rules every refusal is composed from; from `Config`, so a test pins them.
    pub(crate) guardrails: Guardrails,
    /// The left menu: its selection, its collapse set, and what it has marked as open.
    pub sidebar: crate::sidebar::Sidebar,
    pub focus: Focus,
    /// The width of the last frame drawn, recorded by `ui::draw`. `visible` is what the user
    /// asked for and this is what the frame can afford; `menu_shown` is the two together, and
    /// is the one predicate both `ctrl-b` and `tab` act on, so neither can move the keys into
    /// a pane nobody can see.
    pub(crate) width: std::cell::Cell<u16>,
    /// What the last frame laid out, recorded by `ui::draw`. `RefCell` for the reason
    /// `width: Cell<u16>` is one: `draw` takes `&App`, and this is the one place that knows
    /// where every row and every column ended up.
    pub(crate) hits: std::cell::RefCell<crate::mouse::Hits>,
    /// Whether the event loop should capture the mouse. The app owns the wish; the loop owns the
    /// terminal and reconciles to this once per iteration, which is how `:mouse` and `ctrl-o`
    /// take effect without `App` ever touching stdout.
    pub(crate) mouse_on: bool,
    /// Which header the frame draws: what the config file said, and what `:header` writes
    /// back. `Header::Auto` - the default - lets the terminal's height decide, which is what
    /// almost every session wants and nobody has to be told about.
    pub(crate) header: Header,
    /// The interior width of the detail pane on the last frame, recorded by `ui::draw_detail`.
    /// `App::detail_last_line` has to measure the lines the frame drew and cannot reach the
    /// layout; a composed body's line count depends on the width, where a text body's does not.
    ///
    /// The seed covers only the window between opening a pane and its first draw - one `G` at
    /// most - so it is a width the pane can really have in the reference frame rather than the
    /// frame's own.
    pub(crate) detail_width: std::cell::Cell<u16>,
    /// From `Config::snapshot`; read by `ui::meter_readout`, which leaves the meter off a
    /// one-frame run. Not named `snapshot`: `App::snapshot` renders a frame as text and
    /// `Metrics::snapshot` samples the meter, and all three meet in this one call chain.
    pub(crate) one_frame_run: bool,
    /// The Contexts screen: its rows, its add form, its login field. It exists whether or not
    /// its mode is the current one, so `:ctx` opens on what the last visit left behind.
    pub screen: Screen,
    /// Where rows, connections, and removals come from.
    pub(crate) contexts: Box<dyn Contexts>,
    /// Where the contexts come from; the Contexts screen's header names it, so that "no
    /// contexts" and "the wrong config file" are told apart without leaving the program.
    pub(crate) config_path: Option<String>,
    /// The mode the help overlay returns to.
    pub(crate) help_from: Mode,
    /// The mode the palette was opened over: what is drawn behind it and what closing it goes
    /// back to. The palette opens from the table and from the Contexts screen, and `esc` must
    /// return to the one the user was looking at.
    pub(crate) palette_from: Mode,
    /// The mode the `:skin` list was opened over, for the same reason: the palette that opened
    /// it may itself have been over the Contexts screen.
    pub(crate) skins_from: Mode,
    /// The mode `:journal` was opened over, for the same reason again: the journal is a modal
    /// and closing one goes back to what it covered, table or Contexts screen.
    pub(crate) journal_from: Mode,
    /// The mode `:search` was opened over; the same reason a third time.
    pub(crate) search_from: Mode,
    /// The mode the settings screen was opened over; the same reason a fourth time - the menu
    /// opens it, and so does `:settings` from anywhere the palette opens.
    pub(crate) settings_from: Mode,
    /// What the command line named; reopened after a context switch when the session that
    /// follows has nothing else to show.
    pub(crate) home: Home,
    pub(crate) poll_tx: mpsc::Sender<Msg>,
    pub(crate) poll_rx: mpsc::Receiver<Msg>,
    /// Incremented on every `attach`. Messages that are not about a table carry it, so a cycle
    /// that outlived a context switch is dropped - belt and braces beside the abort, because
    /// an abort races a message already in the channel.
    pub(crate) session_generation: u64,
    /// Where this session's cache lives, or `None` for a run with none: `cache = false`,
    /// `--no-cache`, or a context name [`nutsh_core::cache::slug`] refused.
    pub(crate) cache_dir: Option<std::path::PathBuf>,
    /// What the palette has accepted, for `ctrl-p`. Reloaded by `enable_cache`, so a context
    /// switch swaps it: a history is per context by construction, because the context names in
    /// it belong to that context's config and the kinds in it belong to that Prism Central.
    pub(crate) history: nutsh_core::history::History,
    /// Unix seconds of the last write, so the debounce is a subtraction on the existing tick.
    pub(crate) cache_written: u64,
    /// At least one table's generation has advanced since the last write. A write with nothing
    /// to say is a write not worth making.
    pub(crate) cache_dirty: bool,
    /// The stats poller has reported at least once for the current session, so
    /// `settle_stats_once` need not - and must not - wait for the next thirty-second cycle.
    pub(crate) stats_seen: bool,
    /// The warm-up has reported at least once for the current session; the same reason, with a
    /// five-minute cycle to wait out instead of thirty seconds.
    pub(crate) names_seen: bool,
    /// The connect the Contexts screen started, arriving off the UI thread.
    pub(crate) connect_tx: mpsc::Sender<anyhow::Result<Connected>>,
    pub(crate) connect_rx: mpsc::Receiver<anyhow::Result<Connected>>,
    /// The last key, resize, action or context switch. What the idle pause is measured from.
    pub(crate) last_input: std::time::Instant,
    /// `[nav] hide`, read from the contexts seam at startup and rewritten by `:hide`/`:show`.
    pub(crate) nav_hide: Vec<String>,
    /// `[nav] hide_unserved`: whether rule 2 drops what it greys. **Off unless the user set it.**
    pub(crate) nav_hide_unserved: bool,
    /// `:all`: rule 2's dropping is suspended for this session. Rule 1 is not.
    pub(crate) nav_all: bool,
    /// `[refresh]` as the file holds it, re-read after every write so the settings screen's
    /// source column tells the truth on the next frame.
    pub(crate) refresh_cfg: nutsh_core::contexts::Refresh,
    /// `ctrl-t`'s overrides, by kind id: this run only, which is the difference between the key
    /// and the verb. Keyed by **kind** and not by view, so one kind cannot poll at two rhythms
    /// on one screen with nothing on the frame to say which view was retimed.
    pub(crate) refresh_session: HashMap<&'static str, nutsh_core::contexts::Interval>,
    /// Namespaces the settings screen is showing the kinds of. Held here and not in `Settings`,
    /// because every write reopens that screen and a fold that reset on each keystroke would be
    /// worse than no fold at all.
    pub(crate) settings_open: std::collections::BTreeSet<&'static str>,
    pub(crate) quit: bool,
}

pub enum Open {
    Opened,
    Greyed(String),
}

/// An action chosen, with the rows it applies to and what will be sent with them. `rows` is
/// `(extId, name)` in table order.
///
/// Crate-internal, like the `App::pending` that holds it: the menu, the form and the confirm
/// hand work to each other with it, and nothing outside builds one.
pub(crate) struct Pending {
    pub(crate) action: &'static Action,
    pub(crate) rows: Vec<(String, String)>,
    /// The one constant body the catalog curated for this action, sent to every row as it is
    /// written. `acknowledge` and `resolve` are one endpoint told apart by nothing else.
    pub(crate) body: Option<serde_json::Value>,
    /// What the form collected, kept as the strings the user typed rather than as a finished
    /// body: `$ext_id` is *the row's*, so the body is built once per row in `run_pending` and
    /// a bulk cannot send the cursor's id to every one of them.
    pub(crate) entered: Option<HashMap<&'static str, String>>,
}

/// Where the body's cursor is, whichever view it is in.
///
/// Everything the action pipeline needs of a view is here, which is what lets `a` mean the same
/// on a page as on a table: a pane row is a table row, and the only difference is that a pane
/// has no marks.
struct Cursor<'a> {
    key: &'a TableKey,
    selected: usize,
    columns: &'static [Column],
    sort: Option<(usize, bool)>,
    /// The extIds `space` has marked, for a view that has marks at all. `None` on a page's
    /// pane, which marks nothing, so an action there runs on the row under the cursor.
    marks: Option<&'a std::collections::HashSet<String>>,
    /// What `/` has narrowed the view to. `None` on a page's pane, which has no `/` of its own,
    /// and on an unfiltered table.
    filter: Option<&'a nutsh_core::search::Query>,
}

/// What `[KIND]` on the command line named: a kind, or a feature page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    Kind(&'static Kind),
    Page(&'static PageDef),
}

impl Home {
    /// What to call it in a message: the kind's display name, or the page's title.
    pub fn label(self) -> &'static str {
        match self {
            Home::Kind(kind) => kind.display,
            Home::Page(def) => def.title,
        }
    }
}

impl App {
    pub fn view(&self) -> Option<&TableView> {
        self.live.as_ref()?.table()
    }

    pub fn page(&self) -> Option<&PageView> {
        self.live.as_ref()?.page()
    }

    /// The page one below the top, for a table pushed out of a pane.
    pub fn page_under(&self) -> Option<&PageView> {
        let stack = &self.live.as_ref()?.stack;
        match stack.get(stack.len().checked_sub(2)?)? {
            View::Page(p) => Some(p),
            View::Table(_) => None,
        }
    }

    /// Whether the body - the table or the page - has the keyboard. With no menu on screen
    /// there is nothing else for it to be in.
    pub(crate) fn body_focused(&self) -> bool {
        self.focus == Focus::Body || !self.menu_shown()
    }

    /// Whether the view the frame is showing has had its walk stopped by hand.
    pub(crate) fn stopped(&self) -> bool {
        match self.live.as_ref().and_then(|l| l.stack.last()) {
            Some(View::Table(v)) => v.stopped,
            Some(View::Page(p)) => p.focused().is_some_and(|pane| pane.stopped),
            None => false,
        }
    }

    /// Whether nothing is scheduled to poll what the frame is showing.
    ///
    /// A table asks about its own kind. A page asks about every pane that polls, and says
    /// `manual` only when none of them has a schedule: one pane turned off among five that are
    /// still cycling is not a screen that has stopped. The interval itself is never drawn on a
    /// page, because six panes can be six kinds at six rhythms and one number would be a lie
    /// about five of them.
    pub(crate) fn manual(&self) -> bool {
        match self.live.as_ref().and_then(|l| l.stack.last()) {
            Some(View::Table(v)) => self.refresh_of(v.key.kind).0.is_none(),
            Some(View::Page(p)) => {
                let mut polling = p.panes.iter().filter(|pane| pane.sub.is_some()).peekable();
                polling.peek().is_some()
                    && polling.all(|pane| self.refresh_of(pane.key.kind).0.is_none())
            }
            None => false,
        }
    }

    /// The mode drawn behind a modal: the palette, the help overlay and the skin list open over
    /// the Contexts screen as well as over a table.
    pub(crate) fn background_mode(&self) -> Mode {
        match self.mode {
            Mode::Help => self.help_from,
            Mode::Command => self.palette_from,
            Mode::Skins => self.skins_from,
            Mode::Journal => self.journal_from,
            Mode::Search => self.search_from,
            Mode::Settings => self.settings_from,
            // The action pipeline can be started from inside the detail pane, and what a
            // confirm is being read against must stay on the frame.
            Mode::Menu | Mode::Confirm | Mode::Fields => self.acting_from(),
            mode => mode,
        }
    }

    /// The body the action pipeline is running over: the detail pane while one is open, the
    /// table or page otherwise. What the menu, the confirm and the form are drawn on top of,
    /// and where `esc` puts the user back.
    ///
    /// Derived rather than stored, because there is nothing to keep in step: `close_modals`
    /// takes the pane down with every other modal, so a pane exists exactly while it is what
    /// the body shows.
    fn acting_from(&self) -> Mode {
        if self.detail.is_some() {
            Mode::Detail
        } else {
            Mode::Table
        }
    }

    /// Whether the Contexts screen is what the body shows: there is no session, or `:ctx`
    /// opened it over a table.
    pub(crate) fn on_contexts_screen(&self) -> bool {
        self.live.is_none() || matches!(self.background_mode(), Mode::Contexts | Mode::Form)
    }

    /// Whether the menu is on the frame - not whether the user has asked for one. `visible` is
    /// the answer to `ctrl-b`, but below `MENU_SPLIT_COLUMNS` a menu nobody has asked for is
    /// not drawn, and the Contexts screen wants its six columns rather than a menu whose keys
    /// it does not route.
    pub fn menu_shown(&self) -> bool {
        self.sidebar.visible
            && !self.on_contexts_screen()
            && (self.width.get() >= MENU_SPLIT_COLUMNS || self.sidebar.toggled)
    }

    /// The table the body's cursor is in: the top table view, or the focused pane of the open
    /// page. A pane row is a table row, so `y` and `enter` read the row under the cursor the
    /// same way in both.
    fn cursor_table(&self) -> Option<Cursor<'_>> {
        match self.live.as_ref()?.stack.last()? {
            View::Table(view) => Some(Cursor {
                key: &view.key,
                selected: view.selected,
                columns: crate::table::columns(view.key.kind, view.wide),
                sort: view.sort,
                marks: Some(&view.marks),
                filter: view.query(),
            }),
            View::Page(page) => {
                let pane = page.focused()?;
                Some(Cursor {
                    key: &pane.key,
                    selected: pane.selected,
                    columns: pane.columns(),
                    sort: None,
                    marks: None,
                    filter: None,
                })
            }
        }
    }

    /// The kind the body is showing: the detail's when one is open, else the table view's or
    /// the focused pane's. One helper, so `ctrl-t` means the same thing wherever it is pressed.
    pub(crate) fn current_kind(&self) -> Option<&'static Kind> {
        if let Some(key) = self.detail.as_ref().and_then(|d| d.key.as_ref()) {
            return Some(key.kind);
        }
        self.cursor_table().map(|c| c.key.kind)
    }

    /// The extId of the row under the cursor, in the order the frame shows: with a sort
    /// applied the store's order is not the screen's, and `enter` must open the row the user
    /// is looking at.
    pub fn selected_ext_id(&self) -> Option<&str> {
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        let table = live.store.table(cursor.key);
        crate::table::order(
            table,
            cursor.columns,
            live.store.names(),
            self.now,
            cursor.sort,
            cursor.filter,
        )
        .get(cursor.selected)
        .copied()
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// One key. Called from the event loop, so from inside the tokio runtime: the Contexts
    /// screen spawns its connect on it rather than blocking the redraw.
    pub fn handle(&mut self, key: Key) {
        self.dirty = true;
        // First thing, before `ctrl-c` and before the mode dispatch: every key is somebody
        // being there, including the one that quits and the one a modal swallows.
        self.woke();
        // Before the mode dispatch: a modal, a form, or a stuck screen must never be a place
        // where ctrl-c does nothing.
        if key == Key::Ctrl('c') {
            self.quit = true;
            return;
        }
        // One key, one message: what the last key had to say is gone by the time this one
        // draws, unless this one says something of its own.
        self.status = None;
        // The two `q`s have to be consecutive: anything in between is a session that went on
        // doing something else, and the warning it answered is stale.
        if key != Key::Char('q')
            && let Some(live) = self.live.as_mut()
        {
            live.quit_asked = false;
        }
        // Global, because digits are unbound in the body and `tab` means the same thing
        // wherever it is pressed: the menu is the discoverable path and it must be one key
        // away from anywhere a table is.
        if matches!(self.mode, Mode::Table) {
            match key {
                // What is flipped is what is on the frame, not the stored wish: on a frame
                // narrower than `MENU_SPLIT_COLUMNS` the menu starts hidden with `visible`
                // still true, and flipping `visible` would make the advertised `^b menu` a
                // no-op on its first press at exactly the width that most needs it.
                Key::Ctrl('b') => {
                    let shown = self.menu_shown();
                    self.sidebar.visible = !shown;
                    self.sidebar.toggled = true;
                    if shown {
                        self.focus = Focus::Body;
                    }
                    return;
                }
                // The impatient escape from mouse capture: the terminal's own text selection is
                // back on the next frame, this session only. `:mouse` is the one that persists.
                Key::Ctrl('o') => {
                    self.mouse_on = !self.mouse_on;
                    self.status = Some(if self.mouse_on {
                        "mouse on".to_string()
                    } else {
                        "mouse off (shift-drag also selects text in most terminals)".to_string()
                    });
                    return;
                }
                // A page owns `tab`: it has panes of its own to cycle, and a key cannot mean
                // both. `^b` and the digits still reach the menu from one, and `tab` still
                // blurs the menu back to the body.
                Key::Tab
                    if self.focus == Focus::Body && self.menu_shown() && self.page().is_none() =>
                {
                    self.focus_sidebar();
                    return;
                }
                // Whatever has focus, and whatever the sidebar's filter is doing - a digit is
                // unbound in the body and in the menu's own key set, so it can be the one
                // gesture that always means "take me to group N". Not while a term is being
                // typed, in either pane: `10.12` is an address, and a jump to group 1 in the
                // middle of it would be the app disagreeing with the cursor on screen.
                Key::Char(c)
                    if c.is_ascii_digit() && !self.sidebar.filtering && !self.filtering() =>
                {
                    self.focus_menu();
                    self.sidebar.jump(c.to_digit(10).unwrap_or(1));
                    return;
                }
                _ => {}
            }
        }
        // `body_focused`, not `focus`, so the keys follow the same predicate the focused
        // border does: a frame that narrowed under a focused menu until it was no longer drawn
        // gives them back to the table rather than swallowing them.
        if !self.body_focused() && matches!(self.mode, Mode::Table) {
            let action = self.sidebar.key(key);
            self.sidebar_action(action);
            return;
        }
        match self.mode {
            Mode::Table => self.handle_table(key),
            Mode::Command => self.handle_palette(key),
            Mode::Picker => self.handle_picker(key),
            Mode::Detail => self.handle_detail(key),
            Mode::Help => {
                if matches!(key, Key::Esc | Key::Char('?') | Key::Char('q')) {
                    self.mode = self.help_from;
                }
            }
            Mode::Contexts => self.handle_contexts(key),
            Mode::Form => self.handle_form(key),
            Mode::Skins => self.handle_skins(key),
            Mode::Menu => self.handle_menu(key),
            Mode::Confirm => self.handle_confirm(key),
            Mode::Fields => self.handle_fields(key),
            Mode::Journal => self.handle_journal(key),
            Mode::Search => self.handle_search(key),
            Mode::Settings => self.handle_settings(key),
        }
    }

    /// `q` with unfinished watches asks once. `ctrl-c` still quits at once, unconditionally: an
    /// escape hatch that can be blocked is not one. Nothing is cancelled on the way out - the
    /// tasks are Prism Central's, and a later session sees them in the Tasks table.
    fn request_quit(&mut self) {
        let running = self.live.as_ref().map_or(0, |l| l.tasks.running().count());
        let asked = self.live.as_ref().is_some_and(|l| l.quit_asked);
        if running == 0 || asked {
            self.quit = true;
            return;
        }
        if let Some(live) = self.live.as_mut() {
            live.quit_asked = true;
        }
        let plural = if running == 1 { "task" } else { "tasks" };
        self.status = Some(format!(
            "{running} {plural} still running; press q again to quit"
        ));
    }

    /// The rows: the screen owns the keys, the app owns what they need to reach.
    fn handle_contexts(&mut self, key: Key) {
        let action = self.screen.key(key);
        self.screen_action(action);
    }

    fn screen_action(&mut self, action: screen::Action) {
        match action {
            screen::Action::Handled => {}
            screen::Action::Opened => self.mode = Mode::Form,
            screen::Action::Closed => self.mode = Mode::Contexts,
            screen::Action::Reload => self.reload_contexts(),
            screen::Action::Remove(name) => self.remove_context(&name),
            screen::Action::Connect(request, host) => self.start_connect(request, &host),
            screen::Action::Back => {
                if self
                    .live
                    .as_ref()
                    .is_some_and(|live| !live.stack.is_empty())
                {
                    self.mode = Mode::Table;
                }
            }
            screen::Action::Quit => self.quit = true,
            // Without a session there is no kind to open and nothing to quit but `q`.
            screen::Action::Palette => {
                if self.live.is_some() {
                    self.open_palette();
                }
            }
        }
    }

    /// One frame, as the buffer it was drawn into. `snapshot` is its text, so both paths draw
    /// through one code path and a style test cannot drift from a text snapshot.
    ///
    /// Fallible rather than panicking: `--snapshot` is a supported way to run the program, and
    /// a terminal that cannot be built or drawn into is an error to report, not a backtrace.
    pub fn frame(&self, width: u16, height: u16) -> anyhow::Result<ratatui::buffer::Buffer> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|f| crate::ui::draw(self, f))?;
        Ok(terminal.backend().buffer().clone())
    }

    /// One frame as text, trailing spaces trimmed per line.
    pub fn snapshot(&self, width: u16, height: u16) -> anyhow::Result<String> {
        let buffer = self.frame(width, height)?;
        let mut out = String::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        Ok(out)
    }
}

/// How far the wheel moves a selection. Three, because a wheel notch that moved one row would
/// need a wrist and one that moved a page would lose your place.
const WHEEL_ROWS: usize = 3;

/// Which body table an event landed on.
///
/// Read off the hit map's own `pane` field rather than off the entry's position in
/// `Hits::tables`: a pane that says why it has no rows draws no table at all, and a page whose
/// body is too short drops the panes below the fold, so the two part company on the first page
/// with either.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The table view.
    Table,
    /// A pane of the open page, by its index in `PageView::panes`.
    Pane(usize),
}

/// What the pointer landed on, resolved against the recorded layout **before** anything is
/// mutated: `hits` is a `RefCell` and every action below takes `&mut self`.
enum Landed {
    /// An entry of the open modal's list.
    Modal(usize),
    ModalWheel(bool),
    /// A row of the menu, which `enter` would open.
    Menu(usize),
    MenuWheel(bool),
    /// A row of a table or a pane.
    Row {
        target: Target,
        row: usize,
    },
    /// A header of a table or a pane, naming a column of the view's **full** list.
    Header {
        target: Target,
        column: usize,
    },
    /// A pane's block but not its rows: its border, its title, its empty area.
    Block {
        target: Target,
    },
    /// The wheel over a table or a pane; `down` says which way.
    Wheel {
        target: Target,
        down: bool,
    },
    /// `WheelLeft`/`WheelRight` over the table view: the column scroll `←`/`→` does. A pane
    /// has no column scroll - it is drawn at `col_offset: 0` - so the gesture is nothing there.
    Columns {
        right: bool,
    },
    Nothing,
}

/// `by` rows down or up, clamped to `len`. A `len` of zero pins the selection at zero.
fn step(selected: usize, down: bool, by: usize, len: usize) -> usize {
    if down {
        (selected + by).min(len.saturating_sub(1))
    } else {
        selected.saturating_sub(by)
    }
}

/// Everything the next run's first frame needs, cloned off the store.
fn cache_snapshot(live: &Live, now: u64) -> nutsh_core::cache::Snapshot {
    use nutsh_core::cache::{
        CanIRecord, ClusterRecord, Identity, SampleRecord, Snapshot, StatsRecord, TableSnapshot,
    };
    let s = &live.session;
    let stats = live.store.stats();
    let sample = live.store.sample();
    let statuses = s.client.statuses();
    Snapshot {
        identity: Identity {
            host: s.host.clone(),
            port: s.port,
            username: s.username.clone(),
            domain_manager: s.domain_manager.clone(),
            pc_version: s.pc_version.clone(),
        },
        pins: statuses
            .iter()
            .filter_map(|n| Some((n.name.to_string(), n.pinned?.to_string())))
            .collect(),
        namespaces: statuses
            .iter()
            .map(|n| nutsh_core::session::recorded(n, now))
            .collect(),
        cluster: s.cluster.as_ref().map(|c| ClusterRecord {
            ext_id: c.ext_id.clone(),
            name: c.name.clone(),
        }),
        names: live
            .store
            .names()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        stats: Some(StatsRecord {
            clusters: stats.clusters,
            hosts: stats.hosts,
            vms: stats.vms,
            vms_on: stats.vms_on,
            alerts_critical: stats.alerts_critical,
            alerts_warning: stats.alerts_warning,
            tasks_running: stats.tasks_running,
            at: now,
        }),
        sample: Some(SampleRecord {
            policies: sample.policies,
            plans: sample.plans,
            recovery_points: sample.recovery_points,
            jobs: sample.jobs,
            sampled: sample.sampled,
            in_sync: sample.in_sync,
            syncing: sample.syncing,
            out_of_sync: sample.out_of_sync,
            available: sample.available,
            at: now,
        }),
        can_i: live.can_i.get().roles().map(|roles| CanIRecord {
            username: s.username.clone(),
            at: now,
            roles: roles.to_vec(),
        }),
        tables: live
            .store
            .tables()
            // A table that has never polled is the one we restored: writing it back would
            // rewrite yesterday's rows with yesterday's timestamp and never age out.
            .filter(|(_, t)| t.last_poll.is_some() && !t.rows.is_empty())
            // A search's rows are not this session's tables. Its filter was built from what
            // somebody typed once, so keeping it would spend the cache's budget on a question
            // rather than on a view, and hand it back as a table next start.
            .filter(|(key, _)| {
                key.filter.as_deref().is_none_or(|f| {
                    nutsh_catalog::PAGES
                        .iter()
                        .flat_map(|p| p.panes)
                        .any(|pane| pane.filter == Some(f))
                })
            })
            .map(|(key, t)| TableSnapshot {
                kind: key.kind.id.to_string(),
                parents: key.parents.clone(),
                filter: key.filter.as_deref().map(str::to_string),
                // What this run sent. The scheduler narrows the cycle that fills a table, so
                // these rows are that narrowing and labelling them `None` - a whole document -
                // is how a row with no `disks` came back next start as if it had some.
                select: key.kind.select.map(str::to_string),
                total: t.total,
                at: now,
                rows: t.rows.values().map(|e| e.raw.clone()).collect(),
            })
            .collect(),
    }
}

/// Push a table over whatever the stack holds, which stops polling for as long as it is
/// buried: a page under a table is six lists a second for a screen nobody is looking at.
fn push_view(
    live: &mut Live,
    kind: &'static Kind,
    parents: Vec<String>,
    parent_name: Option<String>,
) {
    if let Some(top) = live.stack.last_mut() {
        top.release(&mut live.scheduler, &mut live.store);
    }
    let key = TableKey::under(kind, parents);
    let sub = live.scheduler.subscribe(Subscription::list(
        key.clone(),
        Duration::from_secs(u64::from(kind.poll_secs.max(1))),
    ));
    live.stack.push(View::Table(TableView {
        key,
        sub,
        selected: 0,
        sort: None,
        wide: false,
        col_offset: 0,
        parent_name,
        marks: std::collections::HashSet::new(),
        filter: None,
        stopped: false,
    }));
}

/// Drop the whole stack, unsubscribing everything it polls: what opening a new root does.
fn drain_stack(live: &mut Live) {
    for mut view in std::mem::take(&mut live.stack) {
        match &mut view {
            View::Table(t) => {
                live.scheduler.unsubscribe(t.sub);
                live.store.abandon(&t.key);
            }
            View::Page(p) => release_page(p, &mut live.scheduler, &mut live.store),
        }
    }
}

/// Stop a page polling and end the walks its panes had in flight.
///
/// `PageView::release` aborts the tasks; the store has to be told too, because the `Complete`
/// that would have cleared a pane's in-flight clock dies with the task it was coming from, and
/// a clock nothing ever ends leaves the pane saying `listing…` on every later visit. A buried
/// *table* needs none of this: it is not released, it keeps polling, and its clock stays true.
fn release_page(page: &mut PageView, scheduler: &mut Scheduler, store: &mut Store) {
    page.release(scheduler);
    for pane in &page.panes {
        store.abandon(&pane.key);
    }
}

/// A table's rows as the reference picker lists them: the name to find it by, the extId the
/// body will carry.
fn choices(table: &nutsh_core::store::Table) -> Vec<form::Choice> {
    table
        .rows
        .values()
        .map(|e| form::Choice {
            ext_id: e.ext_id.clone(),
            name: e.name.clone(),
        })
        .collect()
}

/// Why this Prism Central cannot serve `kind`, or `None` when it can. The rule is
/// `Availability`'s own - `nutsh_core::actions` asks the same question of the same method - so
/// the menu, the palette and a refused action always agree.
pub(crate) fn unavailable_reason(session: &Session, kind: &Kind) -> Option<String> {
    session.client.availability(kind).reason()
}

/// The same, for a pane that would otherwise *subscribe* to the kind's list: a 404 the session
/// has already paid for counts here, where it does not above. See `Client::list_availability`
/// for why the two questions are apart - the short of it is that a person opening a table by
/// name keeps `^r`, and a pane polling one on their behalf has nobody to press it.
fn pane_reason(session: &Session, kind: &Kind) -> Option<String> {
    session.client.list_availability(kind).reason()
}

/// Every word of a rest-of-line argument as the one name it spells, or `None` when there was
/// none: `:hide Compute & Storage` names one group, not three things.
fn joined(args: Vec<String>) -> Option<String> {
    (!args.is_empty()).then(|| args.join(" "))
}

/// A configured `[nav] hide` entry resolved to the catalog's own `&'static str`: a kind id or a
/// page id matched exactly, a group name matched case-insensitively because it is display text.
/// An entry that names nothing is ignored here; `:hide` is where a typo is reported, because
/// that is where a person can see the answer.
fn nav_id(entry: &str) -> Option<&'static str> {
    if let Some(k) = nutsh_catalog::kind(entry) {
        return Some(k.id);
    }
    if let Some(p) = nutsh_catalog::page(entry) {
        return Some(p.id);
    }
    // Exactly first, loosely only as a fallback. What this function returns is compared
    // **exactly** by `Sidebar::hides`, and `NAV` holds the curated `Monitoring` beside the
    // generated `monitoring`: resolving case-insensitively and taking whichever twin came first
    // hid the wrong group and could never hide the other one at all.
    let group = |exact: bool| {
        nutsh_catalog::NAV.iter().find(|g| {
            if exact {
                g.name == entry
            } else {
                g.name.eq_ignore_ascii_case(entry)
            }
        })
    };
    group(true).or_else(|| group(false)).map(|g| g.name)
}

/// The command a palette row runs: a command row runs its own, a completed argument runs the one
/// it belongs to, and a kind or a page runs none.
fn entry_command(entry: &Entry) -> Option<Command> {
    match entry {
        Entry::Command(c) => Some(*c),
        Entry::Value {
            tag: palette::Tag::Context,
            ..
        } => Some(Command::Ctx),
        Entry::Value {
            tag: palette::Tag::Skin,
            ..
        } => Some(Command::Skin),
        Entry::Value {
            tag: palette::Tag::Nav { hide },
            ..
        } => Some(if *hide { Command::Hide } else { Command::Show }),
        _ => None,
    }
}

/// `:ctx takes one name`, `:quit takes no argument`, `:can-i takes two arguments`.
///
/// Spelled, not printed as a digit: the user reads all of these from the same status line, and
/// two is the widest command there is, so the `n` arm is the one for a command not yet written.
fn arity_refusal(command: Command, slots: usize) -> String {
    match slots {
        0 => format!(":{} takes no argument", command.label()),
        1 => format!(":{} takes one name", command.label()),
        2 => format!(":{} takes two arguments", command.label()),
        n => format!(":{} takes {n} arguments", command.label()),
    }
}

/// A page whose id or title matches, `lookup`'s best match, a page the word merely spells part
/// of, or an error listing the nearest candidates.
///
/// A page's own name is tried first - `dashboard` is a page and no kind is called that, and a
/// user who types a page's name means the page - and a partial one last, which is the order the
/// palette ranks the two in: `nutsh disaster` and `:disaster` then answer with the same screen,
/// rather than the command line calling a page's name an unknown kind.
pub fn resolve_home(term: &str) -> anyhow::Result<Home> {
    if let Some(page) = nutsh_catalog::PAGES
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(term) || p.title.eq_ignore_ascii_case(term))
    {
        return Ok(Home::Page(page));
    }
    if let Some(kind) = lookup(term).first() {
        return Ok(Home::Kind(kind));
    }
    let lower = term.to_ascii_lowercase();
    if !lower.is_empty()
        && let Some(page) = nutsh_catalog::PAGES.iter().find(|p| {
            p.id.to_ascii_lowercase().contains(&lower)
                || p.title.to_ascii_lowercase().contains(&lower)
        })
    {
        return Ok(Home::Page(page));
    }
    Err(unknown(term, "kind or page"))
}

/// `lookup`'s best match, or an error listing the nearest candidates.
pub fn resolve_kind(term: &str) -> anyhow::Result<&'static Kind> {
    match lookup(term).first() {
        Some(k) => Ok(*k),
        None => Err(unknown(term, "kind")),
    }
}

/// What a term nothing matched deserves: the nearest kind ids, when there are any. A page is
/// never a candidate here - `resolve_home` accepts any page name the term is part of, so a
/// term that reached this point is not part of one.
fn unknown(term: &str, what: &str) -> anyhow::Error {
    let lower = term.to_ascii_lowercase();
    let candidates: Vec<&str> = nutsh_catalog::KINDS
        .iter()
        .filter(|k| {
            k.id.to_ascii_lowercase().contains(&lower)
                || k.display.to_ascii_lowercase().contains(&lower)
        })
        .take(5)
        .map(|k| k.id)
        .collect();
    if candidates.is_empty() {
        return anyhow::anyhow!("unknown {what} {term:?}");
    }
    anyhow::anyhow!(
        "unknown {what} {term:?}; did you mean {}?",
        candidates.join(", ")
    )
}
