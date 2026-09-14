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
    /// A connected app on what `kind` names: a feature page, or a kind's table resolved with
    /// the catalog's `lookup`. Two things are errors rather than an empty screen: a term the
    /// catalog does not know, which reports the nearest candidates, and a kind that resolves
    /// but cannot be opened here, which reports `{display}: {reason}` - `NICs: open from
    /// Virtual Machines` for a child kind, `Virtual Machines: namespace not served` for one
    /// this Prism Central lacks.
    pub fn open(
        session: Session,
        contexts: Box<dyn Contexts>,
        kind: &str,
        config: Config,
    ) -> anyhow::Result<App> {
        let home = resolve_home(kind)?;
        let mut app = App::bare(contexts, home, config, Mode::Table);
        app.attach(session);
        // The rows are read once here, as `disconnected` reads them: the palette completes
        // `:ctx ` from them, and must not have to wait for the Contexts screen to be opened.
        app.reload_contexts();
        match app.reopen(home) {
            Open::Opened => Ok(app),
            Open::Greyed(reason) => Err(anyhow::anyhow!("{}: {reason}", home.label())),
        }
    }

    /// An app with no session: the Contexts screen. `message` is what startup found (a connect
    /// that failed, `no context named X`), shown above whatever reading the rows had to say;
    /// `select` puts the cursor on the row that failed.
    ///
    /// The two go together on purpose. Reading the rows is what clears the message, so a
    /// caller that set one and then moved the cursor would have to set it a second time; here
    /// the rows are read once, before either.
    pub fn disconnected(
        contexts: Box<dyn Contexts>,
        kind: &str,
        message: Option<String>,
        select: Option<&str>,
        config: Config,
    ) -> App {
        // An unknown kind is not worth refusing to start over when there is no session yet:
        // the screen is about contexts, and the command line is corrected from the palette.
        //
        // The binary resolves the kind before it gets here, so today nothing reaches the
        // fallback. It stays because this is a public constructor taking a `&str`: the check
        // belongs with the caller that can report it on the command line, and a caller that
        // skips it must still get a screen rather than a panic.
        let home = resolve_home(kind).unwrap_or_else(|_| {
            Home::Kind(nutsh_catalog::kind("vmm.ahv.config.Vm").expect("catalog has VMs"))
        });
        let mut app = App::bare(contexts, home, config, Mode::Contexts);
        app.reload_contexts();
        // Rule 1 alone here: there is no session, so `statuses()` is empty and rule 2 greys
        // and drops nothing. An unknown answer hides nothing, ever.
        app.apply_nav();
        if let Some(name) = select {
            app.screen.select(name);
        }
        // Beside, not over, whatever reading the rows had to say: a config file that could not
        // be read is the more useful half of "could not connect" when both are true.
        app.screen.message = message;
        app
    }

    /// The fields both constructors share; neither the session nor the rows are loaded yet.
    fn bare(contexts: Box<dyn Contexts>, home: Home, config: Config, mode: Mode) -> App {
        let (poll_tx, poll_rx) = mpsc::channel(64);
        let (connect_tx, connect_rx) = mpsc::channel(4);
        let one_frame_run = config.snapshot;
        // Read before `contexts` is moved into the struct below.
        let nav_hide = contexts.nav_hidden();
        let nav_hide_unserved = contexts.nav_hide_unserved();
        let refresh_cfg = contexts.refresh();
        App {
            live: None,
            mode,
            status: None,
            dirty: true,
            now: config.now,
            palette: None,
            picker: None,
            detail: None,
            skins: None,
            settings: None,
            menu: None,
            confirm: None,
            form: None,
            journal_view: None,
            search: None,
            pending: None,
            guardrails: config.guardrails,
            sidebar: crate::sidebar::Sidebar::default(),
            focus: Focus::Body,
            width: std::cell::Cell::new(REFERENCE_WIDTH),
            hits: std::cell::RefCell::new(crate::mouse::Hits::default()),
            mouse_on: config.mouse,
            header: config.header,
            // The pane's interior in the reference frame: the menu takes 24 cells and the
            // block's own borders two more.
            detail_width: std::cell::Cell::new(REFERENCE_WIDTH - 26),
            one_frame_run,
            screen: Screen::default(),
            contexts,
            config_path: config.config_path,
            help_from: mode,
            palette_from: mode,
            skins_from: mode,
            journal_from: mode,
            search_from: mode,
            settings_from: mode,
            home,
            poll_tx,
            poll_rx,
            session_generation: 0,
            cache_dir: None,
            history: nutsh_core::history::History::default(),
            cache_written: 0,
            cache_dirty: false,
            stats_seen: false,
            names_seen: false,
            connect_tx,
            connect_rx,
            last_input: std::time::Instant::now(),
            nav_hide,
            nav_hide_unserved,
            refresh_cfg,
            refresh_session: HashMap::new(),
            settings_open: std::collections::BTreeSet::new(),
            nav_all: false,
            quit: false,
        }
    }

    /// Replace the session: a fresh store and scheduler, the stack emptied, every modal
    /// closed. The modals matter as much as the stack: subscription ids restart with the new
    /// scheduler, so a detail pane left over from the old session would claim the messages of
    /// whichever view took its id.
    pub(crate) fn attach(&mut self, session: Session) {
        // Before anything else: `self.live` is about to be dropped, and with it the store this
        // write reads. The new context's directory is set by `connected`, which knows its name.
        if self.cache_dirty {
            let _ = self.write_cache_now();
        }
        self.cache_dir = None;
        self.cache_dirty = false;
        // With the directory, because the two are one switch: until `enable_cache` names the
        // incoming context's directory this session has a history it never writes down.
        self.history = nutsh_core::history::History::default();
        self.close_modals();
        self.session_generation += 1;
        // The new session's pollers have not reported yet, whatever the old ones' had done.
        self.stats_seen = false;
        self.names_seen = false;
        let scheduler = Scheduler::new(session.client.clone(), self.poll_tx.clone());
        let stats = nutsh_core::stats::spawn(
            session.client.clone(),
            self.poll_tx.clone(),
            self.session_generation,
            session.cluster.as_ref().map(|c| c.ext_id.clone()),
        );
        let names = nutsh_core::names::spawn(
            session.client.clone(),
            self.poll_tx.clone(),
            self.session_generation,
            // A page of every warmed kind a cycle, for a cache nobody is reading a reference
            // out of: the warm-up is not a `Subscription` either, so it consults the flag
            // itself rather than through a task of the scheduler's.
            scheduler.idle_flag(),
        );
        self.live = Some(Live {
            session,
            store: Store::default(),
            scheduler,
            stack: Vec::new(),
            stats,
            names,
            journal: Journal::default(),
            tasks: TaskIndex::default(),
            can_i: CanICell::default(),
            quit_asked: false,
        });
        self.mode = Mode::Table;
        // After the new `Live`, not before: a context switch is somebody being there, and the
        // scheduler the pause is cleared on has to be the one about to poll.
        self.woke();
        // Last, and after the new `Live`: rule 2 reads this session's negotiation, so the set
        // has to be computed against the client that answered it.
        self.apply_nav();
        self.dirty = true;
    }

    /// Give this session a cache directory and paint whatever was in it. Called between
    /// `App::open` and the first frame, so the rows are there before the event loop starts.
    pub fn enable_cache(
        &mut self,
        dir: std::path::PathBuf,
        restored: Option<nutsh_core::cache::Restored>,
    ) {
        // The Prism Central identified itself as a different one: everything restored is
        // wrong, the store stays empty, and the directory goes. The path stays: this session
        // writes its own inventory into the same place.
        let rejected = self
            .live
            .as_ref()
            .is_some_and(|l| l.session.cache == nutsh_core::session::CacheOutcome::Rejected);
        if rejected {
            let _ = nutsh_core::cache::remove(&dir);
        }
        self.cache_dir = Some(dir);
        // The same directory, so one switch governs both and `nutsh cache clear` removes the
        // history with everything else. Nothing has to be flushed first: every accepted line
        // was written when it was accepted.
        self.history = nutsh_core::history::History::load(self.cache_dir.as_deref());
        let Some(r) = restored.filter(|_| !rejected) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        live.store.apply_names(r.names);
        if let Some(s) = r.stats {
            // Drawn stale: a restored count is by construction not a live one, and the stats
            // poller - which the idle pause never touches - replaces it within thirty seconds.
            live.store.set_stats(nutsh_core::stats::Stats {
                clusters: s.clusters,
                hosts: s.hosts,
                vms: s.vms,
                vms_on: s.vms_on,
                alerts_critical: s.alerts_critical,
                alerts_warning: s.alerts_warning,
                tasks_running: s.tasks_running,
                stale: true,
            });
        }
        if let Some(s) = r.sample {
            live.store
                .set_sample(nutsh_core::sampler::ProtectionSample {
                    policies: s.policies,
                    plans: s.plans,
                    recovery_points: s.recovery_points,
                    jobs: s.jobs,
                    sampled: s.sampled,
                    in_sync: s.in_sync,
                    syncing: s.syncing,
                    out_of_sync: s.out_of_sync,
                    available: s.available,
                });
        }
        if let Some(c) = r.can_i.filter(|c| {
            c.username == live.session.username
                && nutsh_core::cache::now_secs().saturating_sub(c.at)
                    <= nutsh_core::cache::CAN_I_TTL
        }) {
            live.can_i
                .restore(nutsh_core::can_i::CanIndex::with_roles(c.roles));
        }
        let mut painted = 0usize;
        for t in r.tables {
            let key = TableKey {
                kind: t.kind,
                parents: t.parents,
                filter: t.filter.map(std::sync::Arc::from),
            };
            let rows: Vec<nutsh_prism::Entity> = t
                .rows
                .into_iter()
                // Through the *current* catalog: a `name_path` curated since the write is
                // honoured instead of a stale derivation being resurrected.
                .map(|v| nutsh_prism::Entity::new(t.kind, v, None))
                .collect();
            painted += live.store.restore(&key, rows, t.total, t.at);
        }
        tracing::debug!(rows = painted, "cache restored");
        self.cache_written = nutsh_core::cache::now_secs();
        self.dirty = true;
    }

    /// Build the snapshot and hand it to a blocking thread. A frame is never blocked on the
    /// filesystem: the clone costs roughly the serialized size - 750 KB for a 100-VM table -
    /// once a minute, which is much cheaper than a dropped frame.
    pub fn write_cache_now(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let (Some(dir), Some(live)) = (self.cache_dir.clone(), self.live.as_ref()) else {
            return None;
        };
        let now = nutsh_core::cache::now_secs();
        let snapshot = cache_snapshot(live, now);
        self.cache_written = now;
        self.cache_dirty = false;
        // The handle is returned rather than dropped so a test - and the exit path - can wait
        // for the bytes to land. Every other caller ignores it, which is the point: a frame is
        // never blocked on the filesystem.
        Some(tokio::task::spawn_blocking(move || {
            if let Err(e) = nutsh_core::cache::write(&dir, snapshot, now) {
                tracing::debug!(error = %e, "cache write failed");
            }
        }))
    }

    /// Close the palette, the picker and the detail pane, unsubscribing what the detail
    /// polls. A modal belongs to the view under it: when that view is replaced - a new
    /// session, a new root table - a detail left open would go on polling, and its
    /// subscription id could be handed to whichever view came next.
    pub(crate) fn close_modals(&mut self) {
        self.palette = None;
        self.picker = None;
        self.skins = None;
        self.settings = None;
        self.menu = None;
        self.confirm = None;
        self.form = None;
        self.journal_view = None;
        self.search = None;
        self.pending = None;
        self.close_detail();
    }

    /// What the menu greys, and what it drops.
    ///
    /// Greying is unconditional and is computed with `unavailable_reason` - the *same function*
    /// the palette greys an entry with and `open_root` refuses with, so the three surfaces cannot
    /// drift apart. Dropping is rule 1 always, and rule 2 only when `[nav] hide_unserved` is set
    /// and `:all` is off: nothing is hidden unless the user asked for it.
    ///
    /// Event-driven, never per frame: recomputed at connect, on `:hide`/`:show`, on `:all` and on
    /// a context switch, which is exactly when the facts behind it can change. Negotiation has
    /// answered before a `Session` exists, so nothing is greyed on a guess - and with no session
    /// at all nothing is greyed, because an unknown answer hides nothing, ever.
    ///
    /// The walk is over `KINDS` rather than over `statuses()` on purpose: 262 map lookups once
    /// per connect is nothing, and reading the answer out of the one function the other two
    /// surfaces read it out of is the whole point.
    pub(crate) fn apply_nav(&mut self) {
        let mut unserved: HashMap<&'static str, String> = HashMap::new();
        if let Some(live) = self.live.as_ref() {
            for kind in nutsh_catalog::KINDS.iter() {
                if let Some(reason) = unavailable_reason(&live.session, kind) {
                    unserved.insert(kind.id, reason);
                }
            }
        }
        let mut hidden: HashSet<&'static str> = HashSet::new();
        // Rule 1, first and unconditionally: an id in `[nav] hide` is hidden even when its
        // namespace is served, because the user said so.
        for entry in &self.nav_hide {
            if let Some(id) = nav_id(entry) {
                hidden.insert(id);
            }
        }
        // Rule 2, only when asked for: the greyed kinds leave the menu instead of staying dim.
        if self.nav_hide_unserved && !self.nav_all {
            hidden.extend(unserved.keys().copied());
        }
        // The Dashboard is the way back to a screen that works, so it is never hidden - under
        // either of the two names that reach this set. `:hide` refuses both up front; this is
        // the same rule applied to a config file someone edited by hand.
        hidden.retain(|id| !crate::sidebar::protected(id));
        self.sidebar.set_unserved(unserved);
        self.sidebar.set_hidden(hidden);
        self.dirty = true;
    }

    /// `:hide <id|group>` adds an entry and `:show <id>` removes one; both persist through the
    /// same seam `:skin` writes through, so the file's other sections do not move. A failed write
    /// is a status line, not a refusal: the menu already changed, and telling the user it did not
    /// happen would be a lie.
    fn nav_command(&mut self, argument: Option<String>, hide: bool) {
        let word = if hide { ":hide" } else { ":show" };
        let Some(name) = argument.filter(|a| !a.trim().is_empty()) else {
            self.status = Some(format!("{word}: name a kind id, a page id or a group"));
            return;
        };
        let Some(id) = nav_id(name.trim()) else {
            self.status = Some(format!("{name}: no such kind, page or group"));
            return;
        };
        // Refused where it is asked, not silently cancelled where it is applied: saying `hidden
        // Dashboard` and writing an entry that `apply_nav` then ignores is two lies about one
        // instruction, and the dead entry outlives the session.
        if hide && crate::sidebar::protected(id) {
            self.status = Some("the Dashboard cannot be hidden".into());
            return;
        }
        self.nav_hide.retain(|e| nav_id(e) != Some(id));
        if hide {
            self.nav_hide.push(id.to_string());
        }
        self.apply_nav();
        let done = if hide { "hidden" } else { "shown" };
        self.status = Some(match self.contexts.set_nav_hidden(&self.nav_hide) {
            Ok(()) => format!("{done} {id}"),
            Err(e) => format!("{done} {id}; not saved: {e:#}"),
        });
    }

    /// Open `kind` as the root table, dropping the stack and every modal over it. Greyed
    /// kinds explain instead, leaving what is on screen alone.
    pub fn open_root(&mut self, kind: &'static Kind) -> Open {
        if let Some(reason) = reach(kind).reason() {
            return Open::Greyed(reason);
        }
        let Some(live) = self.live.as_ref() else {
            return Open::Greyed("not connected".into());
        };
        if let Some(reason) = unavailable_reason(&live.session, kind) {
            return Open::Greyed(reason);
        }
        self.close_modals();
        let live = self.live.as_mut().expect("a session, checked just above");
        drain_stack(live);
        push_view(live, kind, Vec::new(), None);
        self.mode = Mode::Table;
        self.dirty = true;
        // The menu marks whatever the body shows, however it was opened: a palette jump and a
        // menu `enter` must leave the same row highlighted. Scrolling it into view is `draw`'s
        // job, which frames the selection against the pane it actually has.
        self.sidebar
            .reveal(&nutsh_catalog::NavTarget::Kind(kind.id));
        Open::Opened
    }

    /// Open a feature page as the root view, dropping the stack. A page has no `reach` and no
    /// availability of its own: a pane the session cannot serve says so inside its own block,
    /// which is why the page opens even when every namespace it names is missing.
    pub fn open_page(&mut self, def: &'static PageDef) -> Open {
        if self.live.is_none() {
            return Open::Greyed("not connected".into());
        }
        self.close_modals();
        let live = self.live.as_mut().expect("a session, checked just above");
        drain_stack(live);
        let Live {
            session, scheduler, ..
        } = live;
        let page = PageView::open(def, scheduler, |kind| pane_reason(session, kind));
        live.stack.push(View::Page(page));
        // The invariant `hint_text` rests on, pinned where it is made rather than where it is
        // read: a page is opened onto a drained stack and is therefore always its own root, so
        // `App::back` on a page can only be rule 6 and the prompt line can say `esc:menu`
        // without asking the stack. A future push of a page over a table breaks the hint, and
        // this is what tells whoever writes it.
        debug_assert!(
            matches!(live.stack.as_slice(), [View::Page(_)]),
            "a page is always the root of its stack"
        );
        live.refresh_summary();
        self.start_sampler();
        self.mode = Mode::Table;
        self.dirty = true;
        // As `open_root` does, and for the same reason: the menu marks whatever the body
        // shows, however it was opened - the menu, the palette, or the command line - and a
        // page that revealed nothing would leave the `Kind:` field empty, since the
        // breadcrumb is the menu's path to what is open.
        self.sidebar.reveal(&nutsh_catalog::NavTarget::Page(def.id));
        Open::Opened
    }

    /// Start the open page's own poller, for the page that declares the summary it feeds.
    ///
    /// It is not a subscription - the scheduler knows nothing about it - so it is started here
    /// and stopped by `PageView::release`, wherever the panes are subscribed and released:
    /// `open_page` and `pop`. Idempotent, so a pop onto a page that never buried its sampler
    /// does not start a second one.
    fn start_sampler(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Live {
            session,
            stack,
            scheduler,
            ..
        } = live;
        let Some(View::Page(page)) = stack.last_mut() else {
            return;
        };
        if page.def.summary != Some(crate::page::summary::DISASTER_RECOVERY) || page.sampling() {
            return;
        }
        page.sampler = Some(nutsh_core::sampler::spawn(
            session.client.clone(),
            self.poll_tx.clone(),
            self.session_generation,
            // Fifty-odd gets a cycle, for a block nobody is looking at: the sampler is not a
            // `Subscription`, so it consults the scheduler's flag itself.
            scheduler.idle_flag(),
        ));
    }

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

    /// How often this kind's views poll, and where that came from: the one function, over the
    /// file this app read and the overrides this run has made.
    pub(crate) fn refresh_of(
        &self,
        kind: &'static Kind,
    ) -> (Option<std::time::Duration>, nutsh_core::contexts::Source) {
        nutsh_core::refresh::effective(
            kind,
            &self.refresh_cfg,
            self.refresh_session.get(kind.id).copied(),
        )
    }

    /// `ctrl-t`: one rung down the ladder for the kind of the view in front of the user, for
    /// this session, and every pausable subscription over it retimed on the same keystroke.
    fn step_refresh(&mut self) {
        let Some(kind) = self.current_kind() else {
            return;
        };
        let next = nutsh_core::refresh::step(self.refresh_of(kind).0);
        self.refresh_session.insert(kind.id, next);
        let (every, _) = self.refresh_of(kind);
        if let Some(live) = self.live.as_ref() {
            live.scheduler.set_interval_kind(kind.id, every);
        }
        // The flash names the gesture that keeps it: this is the one place the verb is taught.
        self.status = Some(match every {
            // `cell::span` takes whole seconds and is the one spelling of a length of time this
            // program has, which is why `:refresh 60` comes back as `1m`.
            Some(d) => format!(
                "refresh: every {} · :refresh {} to keep it",
                nutsh_core::cell::span(d.as_secs()),
                d.as_secs()
            ),
            None => "refresh: off · ^r still refreshes once".to_string(),
        });
        self.dirty = true;
    }

    /// `:refresh <auto|off|N>`: the same value, kept in `[refresh.kinds]`. With no argument it
    /// re-opens the palette on the verb, where the ladder is offered.
    fn refresh_command(&mut self, args: &[String]) {
        let Some(kind) = self.current_kind() else {
            return;
        };
        match args.first().map(|w| nutsh_core::refresh::parse(w)) {
            None => self.open_palette_with("refresh "),
            Some(Err(why)) => self.status = Some(format!("refresh: {why}")),
            Some(Ok(v)) => {
                // The file is now where this value lives, so the session override goes: the
                // screen's source column has to say `config file`, not `this session`.
                self.refresh_session.remove(kind.id);
                self.status = Some(
                    match self
                        .contexts
                        .set_interval(nutsh_core::contexts::Schedule::Kind(kind.id), v)
                    {
                        Ok(()) => {
                            self.refresh_cfg = self.contexts.refresh();
                            format!(
                                "refresh: {} kept for {}",
                                nutsh_core::refresh::show(v),
                                kind.display
                            )
                        }
                        Err(e) => format!("refresh not saved: {e:#}"),
                    },
                );
                let (every, _) = self.refresh_of(kind);
                if let Some(live) = self.live.as_ref() {
                    live.scheduler.set_interval_kind(kind.id, every);
                }
            }
        }
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

    /// Tests only: the name of the row under the cursor, which is what a test asserts on and
    /// what a frame already shows.
    #[cfg(feature = "testing")]
    pub fn selected_name(&self) -> Option<&str> {
        let live = self.live.as_ref()?;
        let ext_id = self.selected_ext_id()?;
        let table = live.store.table(self.cursor_table()?.key);
        table.rows.get(ext_id).map(|e| e.name.as_str())
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// A poll message from the scheduler; stale subscriptions are dropped, the rest reach the
    /// store.
    pub fn apply(&mut self, msg: Msg) {
        // Before every arm below, because three of them move `msg` and return: these are the
        // messages that replace what a cache holds - a list cycle's rows, the seven counters,
        // the Disaster Recovery tallies - and the flag is what keeps the debounce from writing
        // the same bytes again a minute later.
        if matches!(
            msg,
            Msg::Complete { .. } | Msg::Stats { .. } | Msg::Sample { .. }
        ) {
            self.cache_dirty = true;
        }
        // Before the gate: an `Acted` belongs to no subscription, so gating it on `is_live`
        // would drop every result whose view had already been popped.
        if let Msg::Acted {
            journal,
            plan,
            result,
        } = msg
        {
            // The user just changed something and is watching for it: that table walks next
            // cycle rather than answering "nothing changed" from a probe.
            // `row_kind`, not `kind`: a refresh is issued against the row's table, not against
            // the kind the request ran on, which is why `PlanRef` carries both.
            let row_kind = plan.row_kind.id;
            // An action is somebody being there, so `woke` comes first. Both, even when it was
            // `woke` that lifted the pause: its `refresh_all` only marks and rings, while
            // `refresh_kind` also respawns a task that has *returned* - the second chance a
            // list that answered 404 gets - and that is the one thing the walk it duplicates
            // does not buy. The duplicate costs a permit `Notify` keeps and one extra cycle of
            // the table the user just acted on, which is the cycle they asked for.
            self.woke();
            if let Some(live) = self.live.as_mut() {
                live.scheduler.refresh_kind(row_kind);
            }
            self.acted(journal, plan, result);
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // A `once` that has run its cycle. The subscription is reaped here, by the receiver,
        // and only after every message that cycle sent has been drained: reaping it on the
        // sender side would race this drain and could gate out the refresh the `once` exists
        // to deliver.
        if let Msg::Done { epoch, sub } = msg {
            live.scheduler.reap(epoch, sub);
            self.search_answered(sub);
            return;
        }
        // Not about a table: the page's sampler, whose cycle belongs to the session that
        // started it and to no subscription at all.
        if let Msg::Sample { generation, sample } = msg {
            if generation == self.session_generation {
                live.store.set_sample(sample);
                live.refresh_summary();
                self.dirty = true;
            }
            return;
        }
        if let Msg::Stats { generation, stats } = msg {
            if generation == self.session_generation {
                live.store.set_stats(stats);
                live.refresh_summary();
                self.stats_seen = true;
                self.dirty = true;
            }
            return;
        }
        if let Msg::Names { generation, names } = msg {
            if generation == self.session_generation {
                live.store.apply_names(names);
                self.names_seen = true;
                self.dirty = true;
            }
            return;
        }
        // `Msg::Sample`, `Msg::Stats`, `Msg::Names` and `Msg::Acted` are the only arms without a
        // subscription, and with `Msg::Done` the only ones without an update; all five returned
        // above. This guard and the `into_update` one below are exhaustive by construction,
        // kept as the routing they read as.
        let Some(sub) = msg.sub() else { return };
        if !live.scheduler.is_live(sub) {
            return; // a message from a view that was popped
        }
        // The detail's single subscription shares its table's key. Only its `Entity` reaches
        // the store, so the row updates in place under the pane; its `Error` belongs to the
        // pane alone, so one missing entity never marks the whole table failed; and it stages
        // no pages, so its `Complete` says nothing.
        if let Some(detail) = self.detail.as_mut()
            && detail.sub == Some(sub)
        {
            match msg {
                Msg::Entity {
                    key,
                    generation,
                    entity,
                    ..
                } => {
                    detail.error = None;
                    live.store.apply(
                        &key,
                        nutsh_core::store::Update::Entity { generation, entity },
                    );
                }
                Msg::Error { error, .. } => detail.error = Some(error),
                // A single subscription never lists, so it never announces a walk either.
                Msg::Started { .. }
                | Msg::Page { .. }
                | Msg::Complete { .. }
                | Msg::Sample { .. }
                | Msg::Stats { .. }
                | Msg::Names { .. }
                | Msg::Acted { .. }
                | Msg::Done { .. } => {}
            }
            self.dirty = true;
            return;
        }
        // A watched task's entity is an ordinary Tasks row, so the watcher reads it off the
        // same channel rather than polling one of its own. Nothing else in the table's own
        // messages can match: only extIds this session asked about are watched.
        let finished = match &msg {
            Msg::Entity { entity, .. } => live.tasks.apply(entity),
            _ => None,
        };
        if let Some(done) = &finished {
            live.scheduler.unsubscribe(done.watch.sub);
            live.journal.settle(
                done.watch.journal,
                done.outcome.clone(),
                Some(done.watch.task.clone()),
            );
        }
        if let Some(done) = finished {
            if matches!(done.watch.status, TaskStatus::Succeeded) {
                self.refresh_row(done.watch.row_kind, &done.watch.ext_id);
            }
            self.status = Some(format!(
                "{} {}: {}",
                done.watch.action,
                done.watch.name,
                done.watch.status.label()
            ));
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // A pane's message belongs to the page, not to a table view: the store still holds the
        // rows, and the summary box is rebuilt from them as they land.
        let on_page = live.page().is_some_and(|page| page.pane_of(sub).is_some());
        let Some((key, update)) = msg.into_update() else {
            return;
        };
        live.store.apply(&key, update);
        live.clamp_selection();
        // A reference picker's one-shot list. It opened on whatever the store held, so the
        // rows that land are folded straight into it, under the filter already typed.
        if let Some(form) = self.form.as_mut()
            && form
                .picker
                .as_ref()
                .is_some_and(|p| key == TableKey::top(p.kind))
        {
            let table = live.store.table(&key);
            form.loading = table.loading;
            // A cycle that failed clears `loading` too, so without this the box would say
            // "no rows" for a kind it never managed to read.
            form.listing_error = table.error.clone();
            let rows = choices(table);
            if let Some(picker) = form.picker.as_mut() {
                picker.set_rows(rows);
            }
        }
        if on_page {
            // The scheduler has already told the client what the server answered, so this asks
            // the session the same question `PageView::open` asked and acts on a different
            // answer: a pane whose list has just 404'd greys and stops polling.
            let Live {
                session,
                scheduler,
                stack,
                ..
            } = live;
            if let Some(View::Page(page)) = stack.last_mut() {
                page.regrade(scheduler, |kind| pane_reason(session, kind));
            }
            live.refresh_summary();
        }
        self.dirty = true;
    }

    /// The result of a mutation: the watch is opened, the journal entry settled, and the row
    /// it changed refreshed out of band.
    fn acted(&mut self, journal: JournalId, plan: PlanRef, result: Result<Outcome, String>) {
        self.dirty = true;
        let outcome = result.unwrap_or_else(Outcome::failed);
        // `live` is borrowed for exactly the work that needs it, and released before the
        // status line and the refresh, which both need `&mut self`.
        let mut refresh = None;
        let status = {
            let Some(live) = self.live.as_mut() else {
                return;
            };
            match &outcome {
                Outcome::Started(task) => {
                    // A watch needs a plan back; `PlanRef` carries everything but the body and
                    // the parents, and a watch never sends anything, so the empty parents are
                    // never used. `row_kind` is not guessed from `kind`: with `action_kind` set
                    // they differ, and the watch refreshes the row's table, not the target's.
                    let full = Plan {
                        kind: plan.kind,
                        ext_id: plan.ext_id.clone(),
                        name: plan.name.clone(),
                        action: plan.action,
                        body: None,
                        parents: Vec::new(),
                        row_kind: plan.row_kind,
                    };
                    let Live {
                        tasks,
                        scheduler,
                        journal: ring,
                        ..
                    } = live;
                    tasks.watch(scheduler, &full, task.clone(), journal);
                    // The id goes in now, not when the watch settles: a running task is exactly
                    // the row a reader wants to look up in `:tasks`, and until the watch
                    // finishes the terminal settle has not run.
                    ring.settle(journal, JournalOutcome::Started, Some(task.ext_id.clone()));
                    format!("{} {}: task started", plan.action.name, plan.name)
                }
                Outcome::Done => {
                    live.journal
                        .settle(journal, JournalOutcome::Succeeded, None);
                    // The row's kind, for the same reason the watch carries it: the refresh is
                    // issued against the table the row is in.
                    refresh = Some((plan.row_kind, plan.ext_id.clone()));
                    format!("{} {}: done", plan.action.name, plan.name)
                }
                // The document is opened over the table below, once `live` is no longer
                // borrowed: an answer nobody can read is an action that reported nothing.
                Outcome::Returned(_) => {
                    live.journal
                        .settle(journal, JournalOutcome::Succeeded, None);
                    format!("{} {}: done", plan.action.name, plan.name)
                }
                Outcome::Failed { failure, forbidden } => {
                    // The journal is exactly where the far end's own words belong: a trace a
                    // developer reads, not a line a person is asked to act on.
                    live.journal.settle(
                        journal,
                        JournalOutcome::Failed(failure.upstream.clone()),
                        None,
                    );
                    // The answer to "may this account act" was just disproved by the server;
                    // the next caller re-resolves rather than going on greying nothing.
                    if *forbidden {
                        live.can_i.invalidate();
                    }
                    format!(
                        "{} {} failed: {}",
                        plan.action.name,
                        plan.name,
                        failure.text()
                    )
                }
            }
        };
        if let Some((kind, ext_id)) = refresh {
            self.refresh_row(kind, &ext_id);
        }
        if let Outcome::Returned(value) = outcome {
            self.open_payload(format!("{} {}", plan.action.title(), plan.name), value);
        }
        self.status = Some(status);
    }

    /// A pane over a document an action answered with. It arrives on the poll channel, so it
    /// can land on any frame: over an open confirm, a half-filled form, a palette mid-word, or
    /// the pane of a row being read. Every one of those is closed with it - `esc` leaves the
    /// payload for the table, and the state behind it has to be the table too - and the pane it
    /// replaces is unsubscribed.
    fn open_payload(&mut self, title: String, value: Value) {
        self.close_modals();
        self.detail = Some(Detail::payload(title, value));
        self.mode = Mode::Detail;
    }

    /// Drop the open pane, unsubscribing what it polled. A payload pane polls nothing, so
    /// closing one unsubscribes nothing.
    fn close_detail(&mut self) {
        if let Some(sub) = self.detail.take().and_then(|d| d.sub)
            && let Some(live) = self.live.as_mut()
        {
            live.scheduler.unsubscribe(sub);
        }
    }

    /// Refresh the affected entity out of band, so the row changes before the next poll cycle.
    /// `kind` is the row's own kind, never the action target's: the `once` writes into the
    /// table the row is in.
    ///
    /// Skipped when that table is not what the top view is showing - the view moved on while
    /// the request was in flight - and skipped when the kind has no `get_path`, which the flat
    /// Host does not: there is no single-entity read to issue. The row settles on the next
    /// ordinary poll instead, and the status line says the same thing either way.
    fn refresh_row(&mut self, kind: &'static Kind, ext_id: &str) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(view) = live.table() else { return };
        if view.key.kind.id != kind.id || kind.get_path.is_none() {
            return;
        }
        let key = view.key.clone();
        live.scheduler
            .subscribe(Subscription::once(key, ext_id.to_string()));
    }

    /// Drains poll messages until every pane of the open page has settled; what `--snapshot`
    /// and the page tests wait for. A page with nothing to poll - every pane's namespace
    /// missing - settles at once.
    ///
    /// Like [`App::settle_once`], it waits forever if a poll never arrives: every caller wraps
    /// it in a timeout.
    pub async fn settle_page_once(&mut self) {
        let Some(page) = self.page() else { return };
        let mut waiting: Vec<SubId> = page.panes.iter().filter_map(|p| p.sub).collect();
        while !waiting.is_empty() {
            let Some(msg) = self.poll_rx.recv().await else {
                return;
            };
            if matches!(&msg, Msg::Complete { .. } | Msg::Error { .. }) {
                waiting.retain(|s| Some(*s) != msg.sub());
            }
            self.apply(msg);
        }
    }

    /// Drains poll messages, applying each, until one of them satisfies `wanted`.
    ///
    /// Like [`App::settle_once`], it waits forever if that message never arrives: every caller
    /// wraps it in a timeout.
    async fn settle_until(&mut self, wanted: fn(&Msg) -> bool) {
        while let Some(msg) = self.poll_rx.recv().await {
            let done = wanted(&msg);
            self.apply(msg);
            if done {
                return;
            }
        }
    }

    /// Drains poll messages until the open page's sampler has reported once.
    pub async fn settle_sample_once(&mut self) {
        self.settle_until(|msg| matches!(msg, Msg::Sample { .. }))
            .await;
    }

    /// Drains poll messages until the stats poller has reported once - and returns at once if
    /// it already has.
    ///
    /// The idempotence is the point: every other settle helper applies every message it
    /// drains, `Msg::Stats` included, so a test that settled a table or a page first may
    /// already hold the cycle. Without this it would wait `PERIOD` - thirty seconds - for the
    /// next one and time out, and which of the two happened would depend on how the requests
    /// interleaved.
    pub async fn settle_stats_once(&mut self) {
        if self.stats_seen {
            return;
        }
        self.settle_until(|msg| matches!(msg, Msg::Stats { .. }))
            .await;
    }

    /// Drains poll messages until the name warm-up has reported once - and returns at once if
    /// it already has, for the reason `settle_stats_once` is idempotent: every other settle
    /// helper applies every message it drains, `Msg::Names` included, so a caller that settled
    /// a table first may already hold the cycle, and without this it would wait `WARM_PERIOD` -
    /// five minutes - for the next one.
    pub async fn settle_names_once(&mut self) {
        if self.names_seen {
            return;
        }
        self.settle_until(|msg| matches!(msg, Msg::Names { .. }))
            .await;
    }

    /// Tests only: the last cycle failed and the values are the previous ones.
    #[cfg(feature = "testing")]
    pub fn inject_stale_stats(&mut self) {
        if let Some(live) = self.live.as_mut() {
            let stats = nutsh_core::stats::Stats {
                stale: true,
                ..*live.store.stats()
            };
            live.store.set_stats(stats);
            self.dirty = true;
        }
    }

    /// Tests only: apply whatever is already in the poll channel, and wait for nothing. What
    /// "two more turns of the event loop" means to a test whose whole point is that nothing
    /// should arrive - `settle_once` would wait forever for a cycle that is never coming.
    #[cfg(feature = "testing")]
    pub async fn drain(&mut self) {
        while let Ok(msg) = self.poll_rx.try_recv() {
            self.apply(msg);
        }
        tokio::task::yield_now().await;
    }

    /// Drains poll messages until the current view's walk has begun and `pages` of its pages
    /// have been applied; `0` returns as soon as the cycle has announced itself.
    ///
    /// `settle_once` waits for the end of a cycle, which is no use to a test that holds the
    /// server open one page at a time: the frame it is about is the one drawn mid-walk.
    pub async fn settle_walk(&mut self, pages: usize) {
        let Some(sub) = self.view().map(|view| view.sub) else {
            return;
        };
        let (mut started, mut seen) = (false, 0);
        while let Some(msg) = self.poll_rx.recv().await {
            let mine = msg.sub() == Some(sub);
            started |= mine && matches!(&msg, Msg::Started { .. });
            seen += usize::from(mine && matches!(&msg, Msg::Page { .. }));
            self.apply(msg);
            // `Started` arrives once, so a caller that waited for it has already taken it off
            // the channel: past zero, pages are the whole of the question.
            let done = if pages == 0 { started } else { seen >= pages };
            if done {
                return;
            }
        }
    }

    /// Drains poll messages until the current table's first `Complete` or `Error`; what
    /// `--snapshot` and tests wait for.
    ///
    /// It never returns on its own if the poll never arrives: `App` holds the sender the
    /// pollers clone, so the channel cannot close while the app is alive and `recv` would wait
    /// forever. Every caller wraps this in a timeout.
    pub async fn settle_once(&mut self) {
        // Which subscription settles, by id rather than by table key: the detail's single
        // subscription shares the key of the table it sits over. An open detail pane is what
        // the next frame shows, so it is the one to wait for; waiting for the table
        // underneath would mean waiting out a whole poll interval for rows already loaded.
        // A payload pane has no subscription of its own, so what settles is the table it was
        // opened over, exactly as if no pane were open.
        let (sub, from_detail) = match self.detail.as_ref().and_then(|d| d.sub) {
            Some(sub) => (sub, true),
            None => match self.view() {
                Some(view) => (view.sub, false),
                None => return,
            },
        };
        while let Some(msg) = self.poll_rx.recv().await {
            let settled = msg.sub() == Some(sub)
                && if from_detail {
                    matches!(&msg, Msg::Entity { .. } | Msg::Error { .. })
                } else {
                    matches!(&msg, Msg::Complete { .. } | Msg::Error { .. })
                };
            self.apply(msg);
            if settled {
                return;
            }
        }
    }

    /// Tests only: mark the current table's last cycle as failed.
    #[cfg(feature = "testing")]
    pub fn inject_error(&mut self, error: &str) {
        if let Some(live) = self.live.as_mut()
            && let Some(key) = live.table().map(|view| view.key.clone())
        {
            let generation = live.store.table(&key).generation + 1;
            live.store.apply(
                &key,
                nutsh_core::store::Update::Error {
                    generation,
                    error: nutsh_core::store::Failure::local(error),
                },
            );
            self.dirty = true;
        }
    }

    /// Tests only: replace the current table with `n` synthetic rows named `vm-000` upwards,
    /// so a test can reach a table taller than any terminal without a fixture that size.
    #[cfg(feature = "testing")]
    pub fn inject_rows(&mut self, n: usize) {
        self.inject_entities(
            (0..n)
                .map(|i| {
                    let name = format!("vm-{i:03}");
                    serde_json::json!({ "extId": name, "name": name })
                })
                .collect(),
        );
    }

    /// Tests only: a listing cycle in flight on the current table, `staged` rows already staged
    /// out of `total`, begun `ago` ago.
    ///
    /// The elapsed time is asserted by moving the *start*, not by freezing a clock: `App::now` is
    /// a `SystemTime` and a cycle's clock is an `Instant`, and the two cannot be the same one - a
    /// `SystemTime` cannot measure an elapsed time and an `Instant` cannot be pinned to a date.
    /// `cell::span` truncates to whole seconds, so `· 3s` is stable for any test that does not
    /// itself take a second.
    #[cfg(feature = "testing")]
    pub fn inject_walking(&mut self, staged: usize, total: u64, ago: std::time::Duration) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(key) = live.table().map(|view| view.key.clone()) else {
            return;
        };
        let kind = key.kind;
        let generation = live.store.table(&key).generation + 1;
        live.store
            .apply(&key, nutsh_core::store::Update::Started { generation });
        // Unconditionally, `staged` of 0 included: an empty first page carrying the server's
        // total is exactly what a large collection answers with, and it is the only way
        // `[0/181825]` - the frame this phase exists to produce - is reachable at all.
        let entities = (0..staged)
            .map(|i| {
                let name = format!("staged-{i:04}");
                nutsh_prism::Entity::new(
                    kind,
                    serde_json::json!({ "extId": name, "name": name }),
                    None,
                )
            })
            .collect();
        live.store.apply(
            &key,
            nutsh_core::store::Update::Page {
                generation,
                entities,
                total: Some(total),
            },
        );
        // After the `Page`, because `Update::Page` does not touch the clock and
        // `Update::Started` set it to *now*: the backdated value is what the frame must read.
        let started = std::time::Instant::now()
            .checked_sub(ago)
            .expect("the process has been up longer than `ago`");
        live.store.set_cycle_started(&key, (generation, started));
        self.dirty = true;
    }

    /// Tests only: replace the current table with these raw entities, so a style test can put
    /// one row per status role on screen without a fixture per role.
    #[cfg(feature = "testing")]
    pub fn inject_entities(&mut self, raw: Vec<serde_json::Value>) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(key) = live.table().map(|view| view.key.clone()) else {
            return;
        };
        let kind = key.kind;
        let total = Some(u64::try_from(raw.len()).unwrap_or(u64::MAX));
        let entities = raw
            .into_iter()
            .map(|r| nutsh_prism::Entity::new(kind, r, None))
            .collect();
        let generation = live.store.table(&key).generation + 1;
        live.store.apply(
            &key,
            nutsh_core::store::Update::Page {
                generation,
                entities,
                total,
            },
        );
        live.store
            .apply(&key, nutsh_core::store::Update::Complete { generation });
        live.clamp_selection();
        self.dirty = true;
    }

    /// Somebody is there. Clears the idle pause and runs every subscription immediately, so
    /// the pause is invisible except for `⏸ idle` and the falling rate - the waking key is
    /// also delivered as an ordinary key, and there is no "press any key to resume" mode.
    ///
    /// The one clock read left in `App`, and it has to be: a key carries no time, and neither
    /// does an action landing. What is *measured* from the stamp is [`App::tick`]'s decision,
    /// and that reads the clock it is handed.
    pub(crate) fn woke(&mut self) {
        self.last_input = std::time::Instant::now();
        if let Some(live) = self.live.as_ref()
            && live.scheduler.is_idle()
        {
            live.scheduler.set_idle(false);
            live.scheduler.refresh_all();
            self.dirty = true;
        }
    }

    /// The second. `now` is what the frame dates itself by; `mono` is the monotonic read the
    /// idle pause is measured on, handed in rather than taken so that entering the pause is
    /// asserted without waiting five minutes for it.
    pub fn tick(&mut self, now: SystemTime, mono: std::time::Instant) {
        self.now = now;
        // The other direction, on the second this already runs on: `woke` is what ends the
        // pause, and this is what starts it.
        if let Some(live) = self.live.as_ref() {
            let idle = nutsh_core::scheduler::idle_state(self.last_input, mono);
            if idle != live.scheduler.is_idle() {
                live.scheduler.set_idle(idle);
                self.dirty = true;
            }
        }
        // A pane redrawn taller has to fetch the rows it can now show, and a tick is where a
        // resize has settled by.
        self.resize_panes();
        // Sixty seconds, and only when something changed. A `kill -9` loses at most a minute,
        // which is the right trade for never blocking a frame.
        if self.cache_dirty
            && nutsh_core::cache::now_secs().saturating_sub(self.cache_written) >= CACHE_PERIOD
        {
            let _ = self.write_cache_now();
        }
        self.dirty = true;
    }

    /// Re-subscribe the open page's panes at the budget the last frame's heights ask for.
    fn resize_panes(&mut self) {
        let Some(Live {
            stack, scheduler, ..
        }) = self.live.as_mut()
        else {
            return;
        };
        let Some(View::Page(page)) = stack.last_mut() else {
            return;
        };
        page.resize(scheduler);
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

    fn sidebar_action(&mut self, action: crate::sidebar::Action) {
        match action {
            crate::sidebar::Action::Moved
            | crate::sidebar::Action::Filtered
            | crate::sidebar::Action::Toggled
            | crate::sidebar::Action::Ignored => {}
            crate::sidebar::Action::Blur => self.focus = Focus::Body,
            // The menu binds a dozen keys; the rest still belong to the body, which is what
            // the prompt line under the menu goes on advertising.
            crate::sidebar::Action::Passthrough(key) => self.handle_table(key),
            crate::sidebar::Action::Open(item) => self.open_target(item),
        }
    }

    /// What a menu item opens. The keys go back to the body either way: the user asked for a
    /// table, not for another turn in the menu.
    ///
    /// The item travels whole rather than as a label: two groups may carry the same label -
    /// the curated `VMs` and the generated `vmm` one both do - so its `note` cannot be looked
    /// up afterwards without risking the wrong item's.
    pub(crate) fn open_target(&mut self, item: &'static nutsh_catalog::NavItem) {
        let (target, label) = (item.target, item.label);
        self.focus = Focus::Body;
        match target {
            nutsh_catalog::NavTarget::Kind(id) => {
                let Some(kind) = nutsh_catalog::kind(id) else {
                    self.status = Some(format!("{label}: not in the catalog"));
                    return;
                };
                // `open_root` reveals what it opened, so the marker follows on its own.
                if let Open::Greyed(reason) = self.open_root(kind) {
                    self.status = Some(format!("{label}: {reason}"));
                }
            }
            nutsh_catalog::NavTarget::Page(id) => {
                let Some(def) = nutsh_catalog::page(id) else {
                    self.status = Some(format!("{label}: not in the catalog"));
                    return;
                };
                // `open_page` reveals what it opened, so the marker follows on its own.
                if let Open::Greyed(reason) = self.open_page(def) {
                    self.status = Some(format!("{label}: {reason}"));
                }
            }
            nutsh_catalog::NavTarget::Contexts => {
                self.sidebar.reveal(&target);
                self.show_contexts(None);
            }
            nutsh_catalog::NavTarget::Settings => {
                self.sidebar.reveal(&target);
                self.show_settings();
            }
            nutsh_catalog::NavTarget::Missing => {
                let note = item.note.unwrap_or("not available");
                self.status = Some(format!("{label}: {note}"));
            }
        }
    }

    fn handle_table(&mut self, key: Key) {
        // A page is a view like a table, and the keys that reach the body reach it first: it
        // binds a smaller set, since a pane sorts and widens nothing.
        if self.page().is_some() {
            self.handle_page(key);
            return;
        }
        // Before every table key, because `j` is a letter while a term is being typed. Only
        // while it is being typed: `enter` hands the keys back and leaves the term standing.
        if self.filtering() {
            self.filter_key(key);
            return;
        }
        match key {
            Key::Char(':') => self.open_palette(),
            Key::Char('/') => self.open_filter(),
            Key::Enter => self.drill(),
            Key::Esc => self.back(),
            Key::Char(' ') => self.toggle_mark(),
            Key::Char('a') => self.open_menu(),
            Key::Char('q') => self.request_quit(),
            Key::Char('y') => self.open_detail(),
            Key::Char('S') => self.cycle_sort(),
            Key::Char('w') => self.toggle_wide(),
            Key::Char('?') => {
                self.help_from = self.mode;
                self.mode = Mode::Help;
            }
            // Nothing is hard-coded: the overlay is what makes `p`, `P`, `r`, `m`, `C`, `s`
            // and `ctrl-d` mean anything, and another kind binds its own. Resolved in the
            // fall-through rather than in a guard, so the lookup - which allocates the key's
            // name and scans the kind's actions - runs once per keypress, and the bound keys
            // above still win: `RESERVED_KEYS` is what keeps a kind from curating one of them.
            _ => match self.curated_key(key) {
                Some(action) => self.start(action),
                None => self.move_in_table(key),
            },
        }
    }

    /// Whether a table's `/` term is taking the keys. Read before the digits and before every
    /// table key, so there is one predicate and not two.
    pub(crate) fn filtering(&self) -> bool {
        self.view()
            .and_then(|v| v.filter.as_ref())
            .is_some_and(|f| f.typing())
    }

    /// `/`: an empty term with the keys in it, over the table the body is showing. Pressed
    /// again over a standing filter it re-opens the term for editing rather than clearing it,
    /// which is what `esc` is for.
    fn open_filter(&mut self) {
        let Some(view) = self.live.as_mut().and_then(Live::table_mut) else {
            return;
        };
        match view.filter.as_mut() {
            Some(filter) => filter.reopen(),
            None => view.filter = Some(Box::new(crate::table::Filter::new())),
        }
    }

    /// One key while a term is being typed. `esc` takes the filter away, `enter` hands the keys
    /// back to the table and leaves it standing, and everything the term does not own still
    /// moves the cursor - so `↑`/`↓` walk the rows that match while they are still being
    /// narrowed.
    fn filter_key(&mut self, key: Key) {
        let Some(view) = self.live.as_mut().and_then(Live::table_mut) else {
            return;
        };
        match key {
            Key::Esc => {
                view.filter = None;
                return;
            }
            Key::Enter => {
                if let Some(filter) = view.filter.as_mut() {
                    filter.commit();
                }
                return;
            }
            _ => {}
        }
        let owned = view
            .filter
            .as_mut()
            .and_then(|filter| filter.key(key))
            .is_some();
        if !owned {
            self.move_in_table(key);
            return;
        }
        // The rows under the term just changed; the cursor cannot stand past the end of what
        // is left. `clamp_selection` is the one place that arithmetic lives.
        if let Some(live) = self.live.as_mut() {
            live.clamp_selection();
        }
    }

    /// The kind's action with this key, in the spelling `RESERVED_KEYS` uses.
    fn curated_key(&self, key: Key) -> Option<&'static Action> {
        let name = match key {
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("ctrl-{c}"),
            _ => return None,
        };
        let target = self.view()?.key.kind.action_target();
        target.actions.iter().find(|a| a.key == name && !a.hidden)
    }

    /// `space`: mark the row under the cursor and move down one.
    ///
    /// The set lives as long as the view, minus two ways out: `esc` clears it, and so does the
    /// action it was gathered for, once the requests are away - a mark that survived its own
    /// bulk action would silently re-run on the same rows at the next keypress.
    fn toggle_mark(&mut self) {
        let Some(ext_id) = self.selected_ext_id().map(str::to_string) else {
            return;
        };
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.table_mut()
            && !view.marks.remove(&ext_id)
        {
            view.marks.insert(ext_id);
        }
        self.move_in_table(Key::Char('j'));
    }

    /// What `esc` does in the body, in precedence order (§7.1). The first rule that matches
    /// wins, and the rules above these are handled before it is reached: a modal closes
    /// itself, and the sidebar's `/` filter and its blur are `Sidebar::key`'s.
    fn back(&mut self) {
        // Rule 4: marks on the current view, cleared instead of popping, so `esc` never
        // surprises.
        if self.clear_marks() {
            return;
        }
        // Rule 4b: a standing `/` term. After the marks and before the pop, so `esc` unwinds in
        // the order the user built: mark a row inside a filtered view and the marks go first,
        // then the filter, then the view.
        if self.clear_filter() {
            return;
        }
        // Rule 5: a view to pop.
        if !self.at_root() {
            self.pop();
            return;
        }
        // Rule 6: a root table or a page. `esc` says "take me back", and from the root the
        // only place back is the menu.
        self.focus_menu();
    }

    /// Drop the current table's `/` term, and say whether there was one. A filter is a way of
    /// looking at rows and not a place in the stack, so it is undone before anything is popped.
    fn clear_filter(&mut self) -> bool {
        self.live
            .as_mut()
            .and_then(Live::table_mut)
            .is_some_and(|v| v.filter.take().is_some())
    }

    /// Drop the marks of the current table, and say whether there were any. The set does not
    /// outlive what it was gathered for.
    fn clear_marks(&mut self) -> bool {
        self.live
            .as_mut()
            .and_then(Live::table_mut)
            .is_some_and(|v| {
                let had = !v.marks.is_empty();
                v.marks.clear();
                had
            })
    }

    /// Show the menu if it is hidden, and move the focus to it. The digits do exactly this
    /// (`Key::Char(c) if c.is_ascii_digit()`), and for the same reason: a key that advertises
    /// "take me to the menu" must not be a no-op at the width where the menu is not drawn.
    /// One helper, so the two paths cannot diverge.
    pub(crate) fn focus_menu(&mut self) {
        self.sidebar.visible = true;
        self.sidebar.toggled = true;
        self.focus_sidebar();
    }

    /// Hand the keys to the menu. The `/` term stops taking them and stays standing, which is
    /// the one rule that keeps `App::filtering` honest: the term has the keys exactly while the
    /// body does, so a digit pressed in the menu is still a jump to group N, and the prompt
    /// line under a focused menu is the body's hints rather than a cursor nobody is typing at.
    pub(crate) fn focus_sidebar(&mut self) {
        if let Some(filter) = self
            .live
            .as_mut()
            .and_then(Live::table_mut)
            .and_then(|v| v.filter.as_mut())
        {
            filter.commit();
        }
        self.focus = Focus::Sidebar;
    }

    /// Whether the body shows the root of its stack: what `esc` acts on, and what the prompt
    /// line's hint says it will do.
    pub(crate) fn at_root(&self) -> bool {
        self.live.as_ref().is_none_or(|l| l.stack.len() <= 1)
    }

    /// The rows an action applies to: the marks in table order, or the row under the cursor.
    ///
    /// In the order the *view* shows, filter included, so an action runs on rows that are on
    /// the frame. A mark made before the term was narrowed is therefore left out - and the
    /// confirm dialog names every row it is about to send to, so what was left out is read
    /// before anything is sent rather than discovered afterwards.
    fn subjects(&self) -> Vec<(String, String)> {
        let Some(live) = self.live.as_ref() else {
            return Vec::new();
        };
        let Some(cursor) = self.cursor_table() else {
            return Vec::new();
        };
        let table = live.store.table(cursor.key);
        let ordered = crate::table::order(
            table,
            cursor.columns,
            live.store.names(),
            self.now,
            cursor.sort,
            cursor.filter,
        );
        let wanted: Vec<&str> = match cursor.marks.filter(|m| !m.is_empty()) {
            None => ordered.get(cursor.selected).copied().into_iter().collect(),
            Some(marks) => ordered
                .into_iter()
                .filter(|id| marks.contains(*id))
                .collect(),
        };
        wanted
            .into_iter()
            .filter_map(|id| {
                table
                    .rows
                    .get(id)
                    .map(|e| (e.ext_id.clone(), e.name.clone()))
            })
            .collect()
    }

    /// Every action that runs on the row under the cursor, in the order every surface lists
    /// them, each greyed by the one reason string.
    ///
    /// **The one list.** The `a` menu, the `⏎` picker's actions group and the detail pane's
    /// actions section all draw it, so the three can never drift apart - and the permission
    /// logic is written once, here.
    ///
    /// The two surfaces that say they are showing what can be done to *this row* take
    /// [`App::entity_action_rows`] instead; this one is the whole set, collection-level
    /// `create` included, because the menu is where creating a thing has to stay reachable.
    fn action_rows(&self) -> Vec<menu::Row> {
        let Some(cursor) = self.cursor_table() else {
            return Vec::new();
        };
        // Once, not once per row: `subjects` orders the whole table and `selected_entity`
        // finds the cursor's row in it, and the list asks the same question of every action
        // the kind has.
        let count = self.subjects().len().max(1);
        let entity = self.selected_entity();
        let mut rows: Vec<menu::Row> = cursor
            .key
            .kind
            .action_target()
            .actions
            .iter()
            .filter(|a| !a.hidden)
            .map(|action| menu::Row {
                action,
                reason: self.refusal_for(action, count, entity),
            })
            .collect();
        menu::sort(&mut rows);
        rows
    }

    /// The part of [`App::action_rows`] that acts on the row under the cursor.
    ///
    /// The `⏎` picker and the detail pane both say, in so many words, that they are showing
    /// what can be done to one entity, so a collection-level operation there would be a lie
    /// about that entity: `create` makes a *new* VM and does nothing to `web-01`. The filter is
    /// the catalog's own [`Action::acts_on_a_row`], so a curated workflow that POSTs to another
    /// kind's collection naming this row - `Create recovery point` - stays where it belongs.
    fn entity_action_rows(&self) -> Vec<menu::Row> {
        let mut rows = self.action_rows();
        rows.retain(|row| row.action.acts_on_a_row());
        rows
    }

    /// `a`: every action of the row kind's `action_target`, greyed by the one reason string.
    fn open_menu(&mut self) {
        // Before the rows are built, not after: the menu is the discoverable path, and it
        // greys nothing on this account's roles until the four list calls have landed. Started
        // here, they are in flight while the menu is being read, so the reasons are right by
        // the time `enter` is pressed.
        self.start_can_i();
        let Some(kind) = self.cursor_table().map(|c| c.key.kind) else {
            return;
        };
        let subjects = self.subjects();
        let rows = self.action_rows();
        if rows.is_empty() {
            self.status = Some("no actions for this kind".into());
            return;
        }
        let subject = match subjects.as_slice() {
            [(_, name)] => name.clone(),
            // The same wording `start` refuses with, rather than `0 Virtual Machines`: an
            // empty table has no subject, and saying so is what tells the reader why `enter`
            // will do nothing.
            [] => "no row selected".into(),
            many => format!("{} {}", many.len(), kind.display),
        };
        self.menu = Some(Menu::open(subject, rows));
        self.mode = Mode::Menu;
    }

    /// The single string the menu, a direct key and `:can-i` all render: why this action cannot
    /// run, or `None` when it can. `count` is the number of rows it would run on, so the bulk
    /// cap is answered by the same call.
    fn refusal(&self, action: &'static Action, count: usize) -> Option<String> {
        self.refusal_for(action, count, self.selected_entity())
    }

    /// [`App::refusal`] with the cursor's row already in hand too, for a caller asking about
    /// every action of one kind: resolving it orders the whole table, and the menu would
    /// otherwise order it once per action.
    fn refusal_for(
        &self,
        action: &'static Action,
        count: usize,
        entity: Option<&nutsh_prism::Entity>,
    ) -> Option<String> {
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        nutsh_core::actions::reason(
            Policy {
                session: &live.session,
                guardrails: &self.guardrails,
                can_i: &live.can_i.get(),
            },
            cursor.key.kind,
            action,
            &cursor.key.parents,
            entity,
            count,
        )
    }

    /// The row under the cursor, in whichever view has it: the top table, or the focused pane
    /// of the open page.
    fn selected_entity(&self) -> Option<&nutsh_prism::Entity> {
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        let id = self.selected_ext_id()?;
        live.store.table(cursor.key).row(id)
    }

    /// The menu's `enter`, and every curated key.
    fn start(&mut self, action: &'static Action) {
        // Resolution is lazy: it starts the first time the menu, a curated key or `:can-i`
        // needs it, reporting `Unknown` until it lands.
        self.start_can_i();
        let subjects = self.subjects();
        // The count the rows will actually be sent with, and it is already in hand.
        if let Some(reason) = self.refusal(action, subjects.len().max(1)) {
            // Every path that could mutate writes a journal entry, including the ones that
            // never reach the network: `:journal` answers "why did nothing happen" too.
            self.journal_refusal(action, &subjects, &reason);
            self.status = Some(reason);
            return;
        }
        if subjects.is_empty() {
            // An empty table, or a cursor on nothing: `reason` is asked with a count of one and
            // usually has nothing to say about it, so the key would otherwise do and say
            // nothing at all.
            self.status = Some("no row selected".into());
            return;
        }
        // A curated constant body is the whole meaning of the action. `acknowledge` and
        // `resolve` POST to the same `manage-alert` endpoint and are told apart by nothing but
        // this, and both carry the generated form over that endpoint's one enum field: opening
        // it would let `A` send `RESOLVE` on the first `enter`, with no confirm to catch it.
        let body = match action.body.map(serde_json::from_str).transpose() {
            Ok(body) => body,
            Err(e) => {
                self.status = Some(format!("{}: malformed curated body: {e}", action.title()));
                return;
            }
        };
        let asks = body.is_none() && action.takes_body && !action.form.is_empty();
        // Before `subjects` is moved into the plan, and it is the count the body will be sent
        // with: a prefill from the cursor's row is one row's values and nobody else's.
        let prefill = if asks {
            self.prefill(action, subjects.len())
        } else {
            None
        };
        self.pending = Some(Pending {
            action,
            rows: subjects,
            body,
            entered: None,
        });
        // A body first, a confirm after: what the user is confirming has to be what will be
        // sent.
        if asks {
            self.form = Some(Form::over(action.title(), action.form, prefill.as_ref()));
            self.mode = Mode::Fields;
            return;
        }
        self.confirm_or_run();
    }

    /// What an update form starts from: the row's current values, by field name.
    ///
    /// `PUT` and `PATCH` only. Those two replace or amend the entity at the path, so a field
    /// left blank is a field cleared, and starting from the current document is what makes the
    /// form an edit rather than a replacement. A `POST` to `$actions/clone` makes something
    /// new, and seeding it with the source's name would offer a duplicate as the default.
    ///
    /// One row only. A bulk action sends one body to every marked row, and the cursor's values
    /// are nobody's but the cursor's - prefilling there would quietly copy one row over the
    /// rest. It is still an edit, though, and that is the distinction the empty map carries:
    /// marked rows exist, so nothing may be invented over them either.
    ///
    /// `None` is "this is not an edit", which is a different thing from an edit that found no
    /// values: [`Form::over`] invents nothing over an existing entity and seeds a blank form
    /// as it always did, and this is what tells the two apart. Possibly-empty `Some` is the
    /// honest answer for a `PUT` whose row the store cannot produce.
    fn prefill(
        &self,
        action: &'static Action,
        rows: usize,
    ) -> Option<std::collections::HashMap<&'static str, String>> {
        if !matches!(action.method, Method::Put | Method::Patch) {
            return None;
        }
        // Empty rather than `None`, and the difference is a VM's disks. `None` says "not an
        // edit", which puts `Form::over` back on the JSON skeletons and the first enum option
        // - over entities that already exist, on a `PUT` that merges what it is handed. One
        // `[{}]` for `disks` sent to every marked row is the same loss the single-row path was
        // fixed for.
        if rows > 1 {
            return Some(std::collections::HashMap::new());
        }
        Some(
            self.selected_entity()
                .map(|e| nutsh_core::actions::current_values(action.form, e))
                .unwrap_or_default(),
        )
    }

    /// Resolve can-i in the background, once. Connect already probes twenty namespaces; four
    /// more list calls on every start would delay the first frame to buy an answer that never
    /// hides anything.
    fn start_can_i(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        // `claim` hands out the generation this resolution belongs to; `set` drops a result
        // whose generation an `invalidate` has since retired, so a 403 that lands while the
        // four lists are still in flight is never overwritten by the answer it disproved.
        let Some(generation) = live.can_i.claim() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            live.can_i.set(
                generation,
                nutsh_core::can_i::CanIndex::unknown("no async runtime"),
            );
            return;
        };
        let cell = live.can_i.clone();
        let client = live.session.client.clone();
        let username = live.session.username.clone();
        runtime.spawn(async move {
            cell.set(
                generation,
                nutsh_core::can_i::resolve(&client, &username).await,
            );
        });
    }

    /// `:try <kind>`. The version refusal is a prediction the catalog makes before the server
    /// has had a vote; this is where a person overrules it.
    ///
    /// One request, and the answer stands: a 404 greys the kind again on what the server said,
    /// which is a better reason than the one it replaced, and a 200 is the kind working for
    /// the rest of the session. Nothing retries it and nothing calls it on the user's behalf -
    /// the whole point is that a person decided to spend the request.
    fn try_command(&mut self, args: &[String]) {
        let Some(term) = args.first() else {
            self.status = Some(":try takes a kind".into());
            return;
        };
        let Some(kind) = lookup(term).first().copied() else {
            self.status = Some(format!("no kind matching {term}"));
            return;
        };
        let Some(live) = self.live.as_ref() else {
            self.status = Some("not connected".into());
            return;
        };
        // Only the version prediction is a guess. A namespace that answered at no version and
        // a 404 this session already paid for are answers, and `:try` has nothing to add to
        // either - it would spend a request to be told again.
        if !matches!(
            live.session.client.availability(kind),
            nutsh_prism::Availability::NotServedAtPin { .. }
        ) {
            self.status = Some(match unavailable_reason(&live.session, kind) {
                Some(reason) => format!("{}: {reason}", kind.display),
                None => format!("{} is served: open it by name", kind.display),
            });
            return;
        }
        live.session.client.ask_anyway(kind);
        if let Open::Greyed(reason) = self.open_root(kind) {
            self.status = Some(reason);
            return;
        }
        self.status = Some(format!(
            "asking this Prism Central for {} at {} anyway",
            kind.display, kind.since
        ));
    }

    /// `:can-i <action> <kind>`. It renders exactly what the menu greys a row with, so the two
    /// cannot disagree by construction.
    fn can_i_command(&mut self, args: &[String]) {
        self.start_can_i();
        let [action_name, kind_term] = args else {
            self.status = Some(":can-i takes an action and a kind".into());
            return;
        };
        let Some(kind) = lookup(kind_term).first().copied() else {
            self.status = Some(format!("no kind matching {kind_term}"));
            return;
        };
        let Some(action) = kind.action_target().action(action_name) else {
            self.status = Some(format!("no action {action_name} on {}", kind.display));
            return;
        };
        let Some(live) = self.live.as_ref() else {
            self.status = Some("not connected".into());
            return;
        };
        // The cursor's row answers for the table it is in and for no other kind: a VM row
        // cannot supply a NIC's parents, and pretending it could is the one thing `:can-i`
        // must not do. Asked about another kind, the question is about the kind alone.
        let (parents, entity) = match self.cursor_table() {
            Some(cursor) if cursor.key.kind.id == kind.id => {
                (cursor.key.parents.clone(), self.selected_entity())
            }
            _ => (Vec::new(), None),
        };
        let can_i = live.can_i.get();
        let verdict = nutsh_core::actions::reason(
            Policy {
                session: &live.session,
                guardrails: &self.guardrails,
                can_i: &can_i,
            },
            kind,
            action,
            &parents,
            entity,
            1,
        );
        // `Some` is the same string the menu shows; `None` reports *why it was allowed*, which
        // is the honest answer and the one `:can-i` exists to give. `No` cannot occur here - it
        // would have made `reason` return `Some`.
        let text = match verdict {
            Some(reason) => reason,
            None => match can_i.can(action) {
                CanI::Yes { via } => format!("allowed (via {via})"),
                CanI::Unknown(why) => format!("unknown - {why}"),
                CanI::No { missing } => format!("not permitted: needs {}", missing.join(", ")),
            },
        };
        self.status = Some(format!("{action_name} on {}: {text}", kind.display));
    }

    /// `:journal`. The entries live in `Live`, so without a session there is nothing to list.
    fn journal_command(&mut self) {
        if self.live.is_none() {
            self.status = Some("not connected".into());
            return;
        }
        self.journal_view = Some(JournalView::default());
        self.journal_from = self.mode;
        self.mode = Mode::Journal;
    }

    /// `:search <term>`: every kind already loaded, looked at for one term, and **no requests**.
    ///
    /// The store is the corpus - this session's polls plus whatever the cache painted in before
    /// the first frame - which is a deliberate reach and not a shortcut: going and listing the
    /// two hundred and forty-eight kinds nobody has opened would be a thousand requests and a
    /// minute of waiting for a question the user expected to be instant. What makes it honest
    /// is that the box says so, in numbers, on every result and on none.
    fn search_command(&mut self, term: &str) {
        let Some(live) = self.live.as_mut() else {
            self.status = Some("not connected".into());
            return;
        };
        if term.is_empty() {
            self.status = Some("search: type what to look for".into());
            return;
        }
        let query = nutsh_core::search::Query::new(term);
        let found = nutsh_core::search::across(&live.store, &query);
        let mut view = crate::search::Search::new(term.to_string(), found);

        // Ask the Prism Central for the rest. One cycle per kind, not a schedule: a search is a
        // question asked once, and the rate limiter paces the fan-out the same as everything
        // else. What lands goes in the store under its own filter, so the table a user has open
        // is never replaced by a filtered subset of itself.
        // Not the kinds this Prism Central has already refused: a search asks on the user's
        // behalf, so a 404 the session has already paid for counts, the way it does for a pane.
        let plan: Vec<_> = nutsh_core::search::plan(&query)
            .into_iter()
            .filter(|ask| pane_reason(&live.session, ask.kind).is_none())
            .collect();
        for ask in &plan {
            let key = nutsh_core::store::TableKey::filtered(
                ask.kind,
                ask.filter.as_deref().map(std::sync::Arc::from),
            );
            let mut sub = nutsh_core::scheduler::Subscription::once_list(key);
            sub.filter = ask.filter.clone();
            view.subs.push(live.scheduler.subscribe(sub));
        }
        view.asked = plan.len();

        self.search = Some(view);
        self.search_from = self.mode;
        self.mode = Mode::Search;
    }

    /// A kind the running search asked about has answered: count it and walk the store again,
    /// so results appear as they land rather than all at the end.
    fn search_answered(&mut self, sub: nutsh_core::scheduler::SubId) {
        let Some(view) = self.search.as_mut() else {
            return;
        };
        if !view.subs.contains(&sub) {
            return;
        }
        view.answered += 1;
        let query = nutsh_core::search::Query::new(&view.term);
        if let Some(live) = self.live.as_ref() {
            let found = nutsh_core::search::across(&live.store, &query);
            view.refill(found);
        }
        self.dirty = true;
    }

    fn handle_search(&mut self, key: Key) {
        let Some(view) = self.search.as_mut() else {
            return;
        };
        match view.key(key) {
            crate::search::Action::Moved | crate::search::Action::Ignored => {}
            crate::search::Action::Closed => {
                self.search = None;
                self.mode = self.search_from;
            }
            crate::search::Action::Chose { kind, ext_id } => {
                self.search = None;
                self.mode = self.search_from;
                self.open_hit(kind, &ext_id);
            }
        }
    }

    /// `⏎` on a result: the kind's own table, with the cursor on the row.
    ///
    /// The table and not the detail pane, because "find this VM" ends with the VM in front of
    /// you and every key that acts on one within reach. A kind that cannot be a root - a disk,
    /// a NIC - is refused in the words `open_root` already refuses it in: *open Disks from a
    /// row of Virtual Machines*, which is where it lives.
    ///
    /// The row may not be in the kind's top-level table: the hit could have come from a child
    /// table or from a page pane's filtered one. Then the cursor stays at the top and the walk
    /// `open_root` just started brings the row in. Nothing is fetched to look it up.
    fn open_hit(&mut self, kind: &'static Kind, ext_id: &str) {
        if let Open::Greyed(reason) = self.open_root(kind) {
            self.status = Some(reason);
            return;
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let Some(view) = live.table() else {
            return;
        };
        let at = crate::table::order(
            live.store.table(&view.key),
            crate::table::columns(view.key.kind, view.wide),
            live.store.names(),
            self.now,
            view.sort,
            None,
        )
        .iter()
        .position(|id| *id == ext_id);
        if let (Some(at), Some(view)) = (at, self.live.as_mut().and_then(Live::table_mut)) {
            view.selected = at;
        }
    }

    fn handle_journal(&mut self, key: Key) {
        let last = self.journal_len().saturating_sub(1);
        let Some(view) = self.journal_view.as_mut() else {
            return;
        };
        match view.key(key, last) {
            journal::Action::Closed => {
                self.journal_view = None;
                self.mode = self.journal_from;
            }
            journal::Action::Moved | journal::Action::Ignored => {}
        }
    }

    /// How many entries `:journal` is listing; the cursor is measured off it.
    fn journal_len(&self) -> usize {
        self.live.as_ref().map_or(0, |l| l.journal.entries().len())
    }

    fn journal_refusal(
        &mut self,
        action: &'static Action,
        subjects: &[(String, String)],
        reason: &str,
    ) {
        let now = self.now;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(view) = live.table() else { return };
        let kind = view.key.kind;
        let context = live
            .session
            .context
            .clone()
            .unwrap_or_else(|| "(env)".into());
        for (ext_id, name) in subjects {
            live.journal.record(
                Attempt {
                    context: &context,
                    kind,
                    ext_id,
                    name,
                    action: action.name,
                },
                now,
                JournalOutcome::Denied(reason.to_string()),
            );
        }
    }

    /// Every borrow is resolved into an owned value before anything mutates `self`: the
    /// verdict needs the session and the pending action, and both arms need `&mut self`.
    fn confirm_or_run(&mut self) {
        let Some((action, names)) = self.pending.as_ref().map(|p| {
            (
                p.action,
                p.rows
                    .iter()
                    .map(|(_, n)| n.clone())
                    .collect::<Vec<String>>(),
            )
        }) else {
            return;
        };
        // Asked again, not carried from `start`: a form can sit open while the
        // background can-i resolution lands, and a refusal that arrives in between must not be
        // able to become a send. The count is the pending rows', which is what will be sent.
        if let Some(reason) = self.refusal(action, names.len().max(1)) {
            let rows = self.pending.take().map(|p| p.rows).unwrap_or_default();
            self.journal_refusal(action, &rows, &reason);
            self.status = Some(reason);
            self.confirm = None;
            self.menu = None;
            self.mode = self.acting_from();
            return;
        }
        let Some((kind, verdict)) = self.live.as_ref().and_then(|live| {
            let kind = self.cursor_table()?.key.kind;
            let context = live.session.context.as_deref().unwrap_or("(env)");
            Some((
                kind,
                self.guardrails.verdict(
                    context,
                    kind,
                    action,
                    names.len(),
                    &live.can_i.get().can(action),
                ),
            ))
        }) else {
            return;
        };
        match verdict {
            Verdict::Confirm(strength) => {
                self.confirm = Some(Confirm::open(
                    strength,
                    action.title(),
                    kind.display,
                    &names,
                ));
                self.mode = Mode::Confirm;
            }
            Verdict::Allow => self.run_pending(),
            // Unreachable: the refusal above answered every one of these and returned. Spelled
            // out rather than left to a `_ => self.run_pending()`, so a verdict added later
            // cannot become a send by falling through.
            Verdict::ReadOnly
            | Verdict::Deny(_)
            | Verdict::NotPermitted(_)
            | Verdict::TooMany { .. } => {
                self.pending = None;
                self.confirm = None;
                self.menu = None;
                self.mode = self.acting_from();
            }
        }
    }

    /// One request per subject, each with its own journal entry and task watch; a failure does
    /// not stop the rest.
    ///
    /// Sequential in one spawned task rather than one task per row: ten marked rows firing ten
    /// simultaneous DELETEs is a different thing from a bulk delete, the bucket paces the rate
    /// but does not order them, and the `Acted` messages - the journal's settle order and the
    /// status line - would arrive in whatever order the responses did.
    fn run_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.confirm = None;
        self.menu = None;
        self.mode = self.acting_from();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.status = Some("no async runtime: cannot act from here".into());
            return;
        };
        // Everything `self` owns and the loop needs, taken before `live` is borrowed mutably -
        // the cursor's table included, since `cursor_table` borrows the stack `live` is in.
        let tx = self.poll_tx.clone();
        let now = self.now;
        let Some(key) = self.cursor_table().map(|c| c.key.clone()) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let kind = key.kind;
        let parents = key.parents.clone();
        let context = live
            .session
            .context
            .clone()
            .unwrap_or_else(|| "(env)".into());
        let client = live.session.client.clone();
        // Planned and journalled here, on the UI thread, so the journal holds the rows in table
        // order whatever the network does with them.
        let mut work: Vec<(nutsh_core::actions::Plan, JournalId)> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for (ext_id, _) in &pending.rows {
            let Some(entity) = live.store.table(&key).rows.get(ext_id).cloned() else {
                continue;
            };
            // Built here, per row, rather than once at submit: `$ext_id` and `$now+30d` are
            // substituted against the row the request is for, so ten marked VMs snapshot
            // themselves rather than the one the cursor happened to be on. A curated constant
            // body has nothing to substitute and is sent to every row as it is.
            let body = match pending.entered.as_ref() {
                Some(entered) => match nutsh_core::actions::build_body(
                    pending.action.form,
                    entered,
                    &entity,
                    now,
                ) {
                    Ok(body) => Some(body),
                    Err(e) => {
                        failures.push(e);
                        continue;
                    }
                },
                None => pending.body.clone(),
            };
            let plan =
                match nutsh_core::actions::plan(kind, &entity, pending.action, &parents, body) {
                    Ok(p) => p,
                    Err(e) => {
                        failures.push(e);
                        continue;
                    }
                };
            let journal = live.journal.record(
                Attempt {
                    context: &context,
                    kind,
                    ext_id: &plan.ext_id,
                    name: &plan.name,
                    action: pending.action.name,
                },
                now,
                JournalOutcome::Started,
            );
            work.push((plan, journal));
        }
        let started = work.len();
        if started > 0 {
            // The marks are what this action was gathered for, and they are spent: leaving them
            // set would let the next keypress re-run the same bulk on the same rows. Kept when
            // nothing could be planned, so a refusal leaves the selection to try something else
            // with.
            if let Some(view) = live.table_mut() {
                view.marks.clear();
            }
            runtime.spawn(async move {
                for (plan, journal) in work {
                    let result = nutsh_core::actions::execute(&client, &plan).await;
                    let _ = tx
                        .send(Msg::Acted {
                            journal,
                            plan: plan.to_ref(),
                            result: Ok(result),
                        })
                        .await;
                }
            });
        }
        if let Some(first) = failures.first() {
            self.status = Some(first.clone());
        } else if started > 1 {
            self.status = Some(format!("{started} actions started"));
        }
    }

    fn handle_menu(&mut self, key: Key) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        match menu.key(key) {
            menu::Event::Filtered | menu::Event::Moved | menu::Event::Ignored => {}
            menu::Event::Cancelled => {
                self.menu = None;
                self.mode = self.acting_from();
            }
            // A greyed row is not refused here: `start` composes the same reason the row is
            // showing, so the menu and a direct key can never disagree about why.
            menu::Event::Chose(action) => self.start(action),
        }
    }

    fn handle_confirm(&mut self, key: Key) {
        let Some(confirm) = self.confirm.as_mut() else {
            return;
        };
        match confirm.key(key) {
            confirm::Event::Typed | confirm::Event::Ignored => {}
            confirm::Event::Mismatch => self.status = Some("names do not match".into()),
            confirm::Event::Cancelled => {
                self.confirm = None;
                self.menu = None;
                self.pending = None;
                self.mode = self.acting_from();
            }
            confirm::Event::Run => self.run_pending(),
        }
    }

    /// The form's keys, or the reference picker's while one is open over it: the picker is a
    /// modal of the form, so it takes every key until it closes.
    fn handle_fields(&mut self, key: Key) {
        if self.form.as_ref().is_some_and(|f| f.picker.is_some()) {
            self.handle_ref_picker(key);
            return;
        }
        if self.form.is_none() {
            // No form to type into is no mode to be in - and the action it was opened for goes
            // with it, for the reason `close_form` gives: a pending left behind would let the
            // next `enter` confirm something nobody is looking at.
            self.close_form();
            return;
        }
        let Some(form) = self.form.as_mut() else {
            return;
        };
        match form.key(key) {
            form::Event::Moved | form::Event::Edited | form::Event::Ignored => {}
            form::Event::Cancelled => self.close_form(),
            form::Event::Pick(kind) => self.open_ref_picker(kind),
            form::Event::Submit => self.submit_form(),
        }
    }

    /// `esc` on the form: the action goes with it, and so does the menu that chose it. A
    /// pending left behind would let the next `enter` confirm something nobody is looking at
    /// any more.
    fn close_form(&mut self) {
        self.form = None;
        self.menu = None;
        self.pending = None;
        self.mode = Mode::Table;
    }

    /// `enter` on a `Reference` field: the rows of the kind it names. The store's rows are
    /// shown at once and a one-shot list is subscribed when no cycle has completed for that
    /// table, so the box fills in rather than the key appearing to do nothing.
    fn open_ref_picker(&mut self, id: &'static str) {
        let Some(kind) = nutsh_catalog::kind(id) else {
            self.form_error(format!("{id} is not in the catalog"));
            return;
        };
        // A kind that needs a parent or a query parameter has no list of its own to offer;
        // saying so beats an empty box, and the extId can still be typed by hand.
        let refused = reach(kind).reason().or_else(|| {
            self.live
                .as_ref()
                .and_then(|live| unavailable_reason(&live.session, kind))
        });
        if let Some(reason) = refused {
            self.form_error(format!("{}: {reason}", kind.display));
            return;
        }
        let Some(label) = self
            .form
            .as_ref()
            .and_then(|f| f.fields.get(f.selected))
            .copied()
            .map(nutsh_core::actions::label_of)
        else {
            return;
        };
        let table_key = TableKey::top(kind);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let table = live.store.table(&table_key);
        let loading = table.loading;
        let failed = table.error.clone();
        let rows = choices(table);
        // What the store holds may be a *slice* of the collection rather than all of it, and
        // `loading` cannot say so: a page pane shares this key with the table view of its kind
        // and walks only as far as it draws (`page::subscribe`), and a kind with a catalog
        // budget stops at it. The server's `total` is the one honest measure, so the picker
        // lists again whenever it names more rows than the table has - a box offering twenty
        // of five hundred clusters with nothing on screen to say so is what this avoids.
        let partial = loading
            || table
                .total
                .is_some_and(|total| u64::try_from(table.rows.len()).is_ok_and(|had| total > had));
        // Once per kind per form: `enter`, `esc`, `enter` on the same reference would
        // otherwise put a second page walk over the same table in flight beside the first,
        // because neither `loading` nor a short table changes until the first one completes.
        let listing = partial
            && self
                .form
                .as_mut()
                .is_some_and(|form| form.first_listing_of(id));
        if listing {
            live.scheduler.subscribe(Subscription::once_list(table_key));
        }
        if let Some(form) = self.form.as_mut() {
            // Loading while a listing is in flight, whichever one it is: the table's own first
            // cycle, or the one this call just issued over rows that were a slice. Never true
            // with nothing coming, or the box would spin for ever.
            form.loading = loading || listing;
            form.listing_error = failed;
            form.picker = Some(RefPicker::open(kind, label, rows));
        }
    }

    fn handle_ref_picker(&mut self, key: Key) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let Some(picker) = form.picker.as_mut() else {
            return;
        };
        match picker.key(key) {
            form::Picked::Filtered | form::Picked::Moved | form::Picked::Ignored => {}
            form::Picked::Cancelled => {
                form.picker = None;
                form.loading = false;
            }
            // The extId, not the name: the body carries the reference, and the name is only
            // what made the row findable.
            form::Picked::Chose(ext_id) => {
                form.picker = None;
                form.set_selected_value(ext_id);
            }
        }
    }

    fn form_error(&mut self, message: String) {
        if let Some(form) = self.form.as_mut() {
            form.error = Some(message);
        }
    }

    /// `enter` on the form: the strings it collected are kept on the pending action and the
    /// confirm follows. `build_body`'s refusal stays under the form with nothing sent.
    ///
    /// The body itself is built in `run_pending`, once per row. Only the refusals the user can
    /// still do something about - a required field left empty, a malformed JSON - are worth
    /// finding here, and a body built now would be one row's, cloned onto every other.
    fn submit_form(&mut self) {
        // Owned or `Copy` first, so nothing borrows `self` while the result is applied.
        let (Some(action), Some(entered)) = (
            self.pending.as_ref().map(|p| p.action),
            self.form.as_ref().map(Form::entered),
        ) else {
            return;
        };
        let now = self.now;
        // Checked against the first of the pending rows still in the store, which is what the
        // confirm dialog names. `None` is no session, or every row gone while the form sat
        // open, and either way there is nothing to substitute against.
        let checked = self.live.as_ref().and_then(|live| {
            let view = live.table()?;
            let table = live.store.table(&view.key);
            let pending = self.pending.as_ref()?;
            let entity = pending.rows.iter().find_map(|(id, _)| table.rows.get(id))?;
            Some(nutsh_core::actions::build_body(
                action.form,
                &entered,
                entity,
                now,
            ))
        });
        match checked {
            // Values are never journalled; the strings go straight onto the pending action.
            Some(Ok(_)) => {
                if let Some(p) = self.pending.as_mut() {
                    p.entered = Some(entered);
                }
                self.form = None;
                self.confirm_or_run();
            }
            Some(Err(e)) => self.form_error(e),
            None => self.form_error("the row this action was started on is gone".into()),
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

    /// Tests only: the extIds `space` has marked, sorted so an assertion is stable.
    #[cfg(feature = "testing")]
    pub fn marks(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .view()
            .map(|v| v.marks.iter().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Tests only: the catalog name of the action under the menu's cursor.
    #[cfg(feature = "testing")]
    pub fn menu_selected_action(&self) -> Option<&'static str> {
        Some(self.menu.as_ref()?.current()?.action.name)
    }

    /// Tests only: which header the next frame draws, without a config file to write it to.
    /// The frame tests measure the two heights against each other, and going through
    /// `:header` would also exercise the palette and the contexts seam.
    #[cfg(feature = "testing")]
    pub fn set_header(&mut self, header: Header) {
        self.header = header;
    }

    /// Tests only: the catalog names the menu is listing, in the order it lists them. The
    /// frame shows titles, and a title says nothing about which catalog action produced it.
    #[cfg(feature = "testing")]
    pub fn menu_actions(&self) -> Vec<&'static str> {
        self.menu
            .as_ref()
            .map(|m| m.entries.iter().map(|r| r.action.name).collect())
            .unwrap_or_default()
    }

    /// Tests only: the catalog names in the open picker's actions group, in the order it lists
    /// them. Together with [`App::menu_actions`] and [`App::detail_action_titles`] this is how
    /// "one list, three surfaces" is asserted rather than asserted about.
    #[cfg(feature = "testing")]
    pub fn picker_actions(&self) -> Vec<&'static str> {
        self.picker
            .as_ref()
            .map(|p| {
                p.entries
                    .iter()
                    .filter_map(|e| match e {
                        crate::picker::Entry::Act { action, .. } => Some(action.name),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Tests only: the labels the open detail pane lists under its `ACTIONS` heading, in the
    /// order it lists them. Titles rather than names, because the drawn lines are what this
    /// surface has: it renders no catalog name anywhere.
    #[cfg(feature = "testing")]
    pub fn detail_action_titles(&self) -> Vec<&'static str> {
        self.detail_actions()
            .iter()
            .map(|r| r.action.title())
            .collect()
    }

    /// Tests only: drains poll messages until the open form's reference picker has rows. It
    /// opens on what the store holds and is refilled by the one-shot list it subscribed, and
    /// no ordinary settle waits for that: the list belongs to no view.
    ///
    /// Like [`App::settle_once`], it waits forever if the rows never arrive: every caller
    /// wraps it in a timeout.
    #[cfg(feature = "testing")]
    pub async fn settle_ref_rows(&mut self) {
        if self.form.as_ref().and_then(|f| f.picker.as_ref()).is_none() {
            // Nothing opened the picker, so nothing will ever fill it. Returning says so where
            // the caller's assertion is; waiting would report it as the caller's timeout.
            return;
        }
        while self
            .form
            .as_ref()
            .and_then(|f| f.picker.as_ref())
            .is_none_or(|p| p.entries.is_empty())
        {
            let Some(msg) = self.poll_rx.recv().await else {
                return;
            };
            self.apply(msg);
        }
    }

    /// Drains the poll channel until the first `Acted`.
    ///
    /// Like [`App::settle_once`], it waits forever if that message never arrives: every caller
    /// wraps it in a timeout.
    #[cfg(feature = "testing")]
    pub async fn settle_acted(&mut self) {
        self.settle_until(|msg| matches!(msg, Msg::Acted { .. }))
            .await;
    }

    /// The keys of a page: `tab` between panes, `O` for the focused pane's kind as a full
    /// table, and the cursor keys inside the focused pane.
    fn handle_page(&mut self, key: Key) {
        match key {
            Key::Tab => self.cycle_pane(true),
            Key::BackTab => self.cycle_pane(false),
            Key::Char('O') => self.open_focused_pane(),
            Key::Enter | Key::Char('y') => self.open_detail(),
            // A pane row is a table row: the whole action pipeline reads `App::cursor_table`,
            // which answers for a focused pane as it answers for a table view, so `a` here runs
            // on the same row `⏎` would open. The Dashboard is the launch view and every group
            // opens a page, so this is where most sessions meet an action at all.
            Key::Char('a') => self.open_menu(),
            Key::Esc => self.back(),
            // A page marks nothing and acts on nothing, but `q` means the same everywhere a
            // body is: a quit that depends on which view is open is one nobody trusts.
            Key::Char('q') => self.request_quit(),
            Key::Char(':') => self.open_palette(),
            Key::Char('?') => {
                self.help_from = self.mode;
                self.mode = Mode::Help;
            }
            _ => self.move_in_pane(key),
        }
    }

    /// `tab`/`shift-tab` between the panes of the open page.
    fn cycle_pane(&mut self, forward: bool) {
        if let Some(live) = self.live.as_mut()
            && let Some(page) = live.page_mut()
        {
            page.cycle(forward);
        }
    }

    /// The keys of a pane that only move its cursor or ask for a poll now.
    fn move_in_pane(&mut self, key: Key) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Live { store, stack, .. } = live;
        let Some(View::Page(page)) = stack.last_mut() else {
            return;
        };
        let Some(pane) = page.focused_mut() else {
            return;
        };
        let last = store.table(&pane.key).rows.len().saturating_sub(1);
        let mut flash: Option<&'static str> = None;
        let mut retime = false;
        match key {
            Key::Char('j') | Key::Down => pane.selected = (pane.selected + 1).min(last),
            Key::Char('k') | Key::Up => pane.selected = pane.selected.saturating_sub(1),
            Key::Char('g') | Key::Home => pane.selected = 0,
            Key::Char('G') | Key::End => pane.selected = last,
            Key::PageDown => pane.selected = (pane.selected + 20).min(last),
            Key::PageUp => pane.selected = pane.selected.saturating_sub(20),
            Key::Ctrl('r') => {
                pane.stopped = false;
                if let Some(sub) = pane.sub {
                    live.scheduler.refresh(sub);
                }
            }
            Key::Ctrl('x') => {
                let stopped = pane.sub.is_some_and(|sub| live.scheduler.stop(sub));
                pane.stopped = stopped;
                flash = Some(if stopped {
                    "stopped"
                } else {
                    "nothing to stop"
                });
            }
            // Handled after the borrows end, because `step_refresh` needs `self` whole.
            Key::Ctrl('t') => retime = true,
            _ => {}
        }
        // After the borrows of `live`'s fields end, because both of these need `self` again.
        if let Some(text) = flash {
            self.status = Some(text.to_string());
        }
        if retime {
            self.step_refresh();
        }
    }

    /// `O`: the focused pane's kind as a full table, pushed over the page. The table is the
    /// unfiltered listing - the pane's filter is what made it a pane, not what the kind is.
    ///
    /// Refused for the same two reasons `open_root` refuses: a pane already saying why this
    /// Prism Central cannot serve it would otherwise open a table that polls and fails on a
    /// loop with nothing marking it, and a child kind has no listing of its own to open -
    /// `TableKey::under(kind, vec![])` would leave its parent placeholders unfilled.
    fn open_focused_pane(&mut self) {
        let Some(pane) = self.page().and_then(|p| p.focused()) else {
            return;
        };
        let (kind, title) = (pane.key.kind, pane.def.title);
        let refused = pane.reason.clone().or_else(|| reach(kind).reason());
        if let Some(reason) = refused {
            self.status = Some(format!("{title}: {reason}"));
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        push_view(live, kind, Vec::new(), None);
    }

    /// The keys of a table that only move the cursor or ask for a poll now.
    fn move_in_table(&mut self, key: Key) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Live {
            store,
            stack,
            scheduler,
            ..
        } = live;
        let Some(View::Table(view)) = stack.last_mut() else {
            return;
        };
        // What the frame shows, not what the table holds: `G` on a filtered table goes to the
        // last row that matches.
        let len = crate::table::matching(store.table(&view.key), view.query());
        let last = len.saturating_sub(1);
        let mut flash: Option<&'static str> = None;
        let mut retime = false;
        match key {
            Key::Char('j') | Key::Down => view.selected = (view.selected + 1).min(last),
            Key::Char('k') | Key::Up => view.selected = view.selected.saturating_sub(1),
            Key::Char('g') | Key::Home => view.selected = 0,
            Key::Char('G') | Key::End => view.selected = last,
            Key::PageDown => view.selected = (view.selected + 20).min(last),
            Key::PageUp => view.selected = view.selected.saturating_sub(20),
            // `←`/`→` mean "scroll columns" in a table and "collapse/expand" in the sidebar:
            // the same shape of gesture in both, and never ambiguous, because focus decides.
            Key::Left => view.col_offset = view.col_offset.saturating_sub(1),
            Key::Right => {
                let last =
                    crate::table::max_offset(crate::table::columns(view.key.kind, view.wide));
                view.col_offset = (view.col_offset + 1).min(last);
            }
            // `ctrl-r` is the way out of a stopped view as well as a request for a cycle now:
            // `Scheduler::refresh` clears the subscription's own flag, and this clears the
            // view's.
            Key::Ctrl('r') => {
                view.stopped = false;
                scheduler.refresh(view.sub);
            }
            Key::Ctrl('x') => {
                let stopped = scheduler.stop(view.sub);
                view.stopped = stopped;
                flash = Some(if stopped {
                    "stopped"
                } else {
                    "nothing to stop"
                });
            }
            // Handled after the borrows end, because `step_refresh` needs `self` whole.
            Key::Ctrl('t') => retime = true,
            _ => {}
        }
        // After the borrows of `live`'s fields end, because both of these need `self` again.
        if let Some(text) = flash {
            self.status = Some(text.to_string());
        }
        if retime {
            self.step_refresh();
        }
    }

    /// `S`: sort by the next column, then by that column reversed, then on to the next one;
    /// past the last column reversed, back to the order the store holds.
    fn cycle_sort(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(view) = live.table_mut() else {
            return;
        };
        let columns = crate::table::columns(view.key.kind, view.wide).len();
        view.sort = match view.sort {
            None => (columns > 0).then_some((0, false)),
            Some((col, false)) => Some((col, true)),
            Some((col, true)) if col + 1 < columns => Some((col + 1, false)),
            Some((_, true)) => None,
        };
    }

    /// `w`: up to twenty columns instead of six. The column offset is clamped, since `w` can
    /// narrow the column set under it.
    fn toggle_wide(&mut self) {
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.table_mut()
        {
            view.wide = !view.wide;
            let last = crate::table::max_offset(crate::table::columns(view.key.kind, view.wide));
            view.col_offset = view.col_offset.min(last);
        }
    }

    /// `enter` on a row: a picker of what can be opened under it and what can be done to it, or
    /// the detail pane when the kind has no children.
    fn drill(&mut self) {
        // The picker greys its actions on this account's roles, and a draw cannot start the
        // four list calls that answer for them; `a` does the same for the same reason.
        self.start_can_i();
        let Some(picker) = self.build_picker() else {
            return;
        };
        if picker.is_empty() {
            self.open_detail();
            return;
        }
        self.picker = Some(picker);
        self.mode = Mode::Picker;
    }

    /// The picker for the row under the cursor, greyed by what this session serves; `None`
    /// when there is no row.
    fn build_picker(&self) -> Option<Picker> {
        let actions = self.entity_action_rows();
        let ext_id = self.selected_ext_id()?;
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        let name = live
            .store
            .table(cursor.key)
            .rows
            .get(ext_id)
            .map_or_else(|| ext_id.to_string(), |e| e.name.clone());
        Some(Picker::open(
            cursor.key.kind,
            ext_id.to_string(),
            name,
            |kind| unavailable_reason(&live.session, kind),
            actions,
        ))
    }

    fn handle_picker(&mut self, key: Key) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        match picker.key(key) {
            picker::Event::Filtered | picker::Event::Moved | picker::Event::Ignored => {}
            picker::Event::Cancelled => {
                self.picker = None;
                self.mode = Mode::Table;
            }
            // The same refusal the menu greys the row with, and the same path it runs down:
            // `start` composes the reason again, so the box and a direct key cannot disagree.
            picker::Event::Run(action) => {
                self.picker = None;
                self.mode = Mode::Table;
                self.start(action);
            }
            picker::Event::Chose(child) => {
                let picker = self.picker.take().expect("borrowed above");
                self.mode = Mode::Table;
                // A greyed child has nothing to open; the reason is all this key can offer.
                if let Some(reason) = picker
                    .current()
                    .and_then(|e| e.reason().map(str::to_string))
                {
                    self.status = Some(reason);
                    return;
                }
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                // The child's list path spells out the whole chain, so the row's own extId is
                // appended to the parents the current view already carries.
                let mut parents = live
                    .table()
                    .map(|v| v.key.parents.clone())
                    .unwrap_or_default();
                parents.push(picker.parent_ext_id);
                push_view(live, child, parents, Some(picker.parent_name));
            }
        }
    }

    /// The view underneath the top one, or nothing when there is only one. Rule 5 of what
    /// `esc` does, and [`App::back`] is what decides to call it - at the root it goes to the
    /// menu instead.
    ///
    /// The popped table's rows stay in the store on purpose: drilling into the same row again
    /// then shows what it held while the first poll of the new subscription runs, instead of
    /// an empty screen. An eviction policy can come once
    /// a session can wander through enough tables for their rows to add up.
    fn pop(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if live.stack.len() <= 1 {
            return;
        }
        match live.stack.pop() {
            Some(View::Table(view)) => {
                live.scheduler.unsubscribe(view.sub);
                live.store.abandon(&view.key);
            }
            Some(View::Page(mut page)) => {
                release_page(&mut page, &mut live.scheduler, &mut live.store);
            }
            None => {}
        }
        if let Some(top) = live.stack.last_mut() {
            top.reacquire(&mut live.scheduler);
        }
        // The page's own poller went down with its panes when it was buried; it comes back
        // with them.
        self.start_sampler();
    }

    /// `y`, or `enter` on a kind with no children: the entity, refreshed by a subscription of
    /// its own on the same rhythm as the table. On a page it is the focused pane's row.
    fn open_detail(&mut self) {
        // The pane's actions section greys on this account's roles, and a draw path holds
        // `&App` and cannot start the four list calls that answer for them. Started here, they
        // are in flight while the pane is being read.
        self.start_can_i();
        let Some(ext_id) = self.selected_ext_id().map(str::to_string) else {
            return;
        };
        let Some(key) = self.cursor_table().map(|cursor| cursor.key.clone()) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // No `get_path` means no single-entity read to issue, which the flat Host is the
        // standing example of. Subscribing one anyway spends a cycle to be told so by the
        // catalog and paints `has no get endpoint` across the top of the pane, which is a fact
        // about the catalog and not something to tell the reader in the middle of their frame.
        // The pane is composed from the list row instead, and the list is still polling.
        let sub = key.kind.get_path.map(|_| {
            live.scheduler.subscribe(Subscription::single(
                key.clone(),
                Duration::from_secs(u64::from(key.kind.poll_secs.max(1))),
                ext_id.clone(),
            ))
        });
        let body = detail::Body::default_for(Some(&key));
        self.detail = Some(Detail {
            key: Some(key),
            ext_id,
            sub,
            body,
            scroll: 0,
            error: None,
            payload: None,
            heading: None,
        });
        self.mode = Mode::Detail;
    }

    fn handle_detail(&mut self, key: Key) {
        // Taken before the pane's own keys, and for the reason the pane has an actions section
        // at all: `a` means the same here as on the table, on the row this pane is over. The
        // pane binds no `a`, so nothing is being shadowed.
        if key == Key::Char('a') {
            self.open_menu();
            return;
        }
        // Likewise: `ctrl-t` retimes the kind this pane is over, so the key means the same
        // thing wherever the body is.
        if key == Key::Ctrl('t') {
            self.step_refresh();
            return;
        }
        let last = self.detail_last_line();
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        match detail.key(key, last) {
            detail::Action::Scrolled | detail::Action::Toggled | detail::Action::Ignored => {}
            detail::Action::Help => {
                self.help_from = Mode::Detail;
                self.mode = Mode::Help;
            }
            detail::Action::Closed => {
                self.close_detail();
                self.mode = Mode::Table;
            }
        }
    }

    /// The last line the detail pane can scroll to, measured off the lines it would draw.
    fn detail_last_line(&self) -> u16 {
        let Some(detail) = self.detail.as_ref() else {
            return 0;
        };
        // As `ui::draw_detail` does, and for the same reason: a payload pane reads nothing from
        // the store, so it must not need a session to measure itself.
        let empty = nutsh_core::cell::Names::default();
        let names = self.live.as_ref().map_or(&empty, |l| l.store.names());
        Detail::last_line(&detail.lines(detail::BodyView {
            entity: self.detail_entity(),
            names,
            now: self.now,
            width: self.detail_width.get(),
            actions: &self.detail_actions(),
        }))
    }

    /// The actions the open pane lists under its `ACTIONS` heading: the row's, from the one
    /// list every action surface draws. Nothing for a payload pane - the document an action
    /// answered with is over no row, so there is nothing there to act on.
    ///
    /// Public because `ui::draw_detail` measures the same lines `App::detail_last_line` does,
    /// and the two must be given the same rows.
    pub fn detail_actions(&self) -> Vec<menu::Row> {
        match self.detail.as_ref() {
            Some(detail) if detail.key.is_some() => self.entity_action_rows(),
            _ => Vec::new(),
        }
    }

    /// The row an open pane is over, when it is over one: a payload pane carries its document
    /// and reads nothing from the store. Crate-internal because `ui::draw_detail` measures the
    /// same text this does, and the two must read the same row.
    pub(crate) fn detail_entity(&self) -> Option<&nutsh_prism::Entity> {
        let detail = self.detail.as_ref()?;
        let live = self.live.as_ref()?;
        // `Table::row`, not `rows`: a list narrowed by `$select` holds the columns and nothing
        // else, and the pane is composed from the whole document the detail's own single-entity
        // subscription fetched.
        live.store.table(detail.key.as_ref()?).row(&detail.ext_id)
    }

    fn open_palette(&mut self) {
        self.open_palette_with("");
    }

    /// The palette, opened with `prefix` already typed. One caller so far - the settings
    /// screen's `a`, which lands on a ranked, completing `:hide ` rather than on a second list
    /// that screen would have to keep correct.
    fn open_palette_with(&mut self, prefix: &str) {
        let mut palette = Palette::with_history(self.history.entries().to_vec());
        for c in prefix.chars() {
            palette.key(Key::Char(c));
        }
        self.palette = Some(palette);
        self.refresh_palette();
        self.palette_from = self.mode;
        self.mode = Mode::Command;
    }

    fn handle_palette(&mut self, key: Key) {
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        let action = palette.key(key);
        // The line at the moment `Chose` was produced, trimmed, read before the palette is
        // dropped - and only then: a cancelled line and an `enter` with nothing under the
        // cursor are not things the user committed to.
        let line = matches!(action, palette::Action::Chose(..))
            .then(|| palette.input.as_str().trim().to_string());
        match action {
            // An edit re-ranks, and so does a cursor move: `Palette::refresh` is a function of
            // the cursor, so a cursor that crossed a word boundary is completing a different
            // word. `refresh` only clamps `selected` and never resets it, so re-ranking on the
            // `Moved` that `↑`/`↓` also produce keeps the selection where the user put it. An
            // accepted ghost changed the line, so it re-ranks for the same reason an edit does.
            palette::Action::Edited | palette::Action::Moved | palette::Action::Completed => {
                self.refresh_palette()
            }
            palette::Action::Ignored => {}
            palette::Action::Cancelled => {
                self.palette = None;
                self.mode = self.palette_from;
            }
            palette::Action::Chose(entry, args) => {
                self.palette = None;
                self.mode = self.palette_from;
                // Recorded before the command runs: `:ctx other` swaps the session and with it
                // the history, so a line written after the switch would land in the wrong file.
                if let Some(line) = line {
                    self.history.push(&line);
                }
                self.choose(entry, args);
            }
        }
    }

    /// What `enter` in the palette does with the entry under the cursor. `args` is every word
    /// after the command word: one command takes two of them.
    fn choose(&mut self, entry: Entry, args: Vec<String>) {
        // Before anything else. `args.first()` alone is a silent drop: `:ctx a b` would
        // connect to `a` and say nothing about `b`.
        // The message is built from the command's label and its slot count, so a command added
        // later gets its refusal for free.
        if let Some(command) = entry_command(&entry) {
            let slots = command.slots().len();
            // A nav name is display text: `Compute & Storage` is three words in one slot, so
            // the last slot of `:hide`/`:show` swallows the rest of the line.
            if args.len() > slots && !command.rest_of_line() {
                self.status = Some(arity_refusal(command, slots));
                return;
            }
        }
        match entry {
            Entry::Command(Command::Quit) => self.quit = true,
            Entry::Command(Command::Help) => {
                self.help_from = self.mode;
                self.mode = Mode::Help;
            }
            Entry::Command(Command::Ctx) => self.ctx_command(args.into_iter().next()),
            Entry::Command(Command::Skin) => self.skin_command(args.into_iter().next()),
            Entry::Command(Command::Journal) => self.journal_command(),
            // Joined, not `first()`: the term is the rest of the line, spaces and all.
            Entry::Command(Command::Search) => self.search_command(&args.join(" ")),
            Entry::Command(Command::CanI) => self.can_i_command(&args),
            Entry::Command(Command::Try) => self.try_command(&args),
            Entry::Command(Command::Mouse) => self.mouse_command(),
            Entry::Command(Command::Header) => self.header_command(),
            Entry::Command(Command::Log) => self.log_command(&args),
            Entry::Command(Command::Settings) => self.show_settings(),
            Entry::Command(Command::Refresh) => self.refresh_command(&args),
            Entry::Command(Command::All) => {
                self.nav_all = !self.nav_all;
                self.apply_nav();
                // `:all` suspends rule 2 and nothing else, so it may not claim to be showing
                // every kind while rule 1 - an instruction, not a heuristic - keeps one out.
                // The count is of `[nav] hide` entries, which is what the user typed.
                self.status = Some(match (self.nav_all, self.nav_hide_unserved) {
                    (true, _) if self.nav_hide.is_empty() => {
                        "showing every kind this session".into()
                    }
                    (true, _) => format!(
                        "showing every kind this session except the {} you hid",
                        self.nav_hide.len()
                    ),
                    (false, true) => "hiding what this Prism Central does not serve".into(),
                    (false, false) => "nothing is hidden but what you asked for".into(),
                });
            }
            // Joined, not `first()`: the words are the name, and the menu spells it with spaces.
            Entry::Command(Command::Hide) => self.nav_command(joined(args), true),
            Entry::Command(Command::Show) => self.nav_command(joined(args), false),
            // A completed argument runs the command it belongs to.
            Entry::Value { text, tag } => match tag {
                palette::Tag::Context => self.ctx_command(Some(text)),
                palette::Tag::Skin => self.apply_skin(&text),
                palette::Tag::Nav { hide } => self.nav_command(Some(text), hide),
                palette::Tag::Interval => self.refresh_command(&[text]),
                palette::Tag::Level => self.log_command(&[text]),
            },
            Entry::Kind {
                kind,
                reason: Some(reason),
            } => match reach(kind) {
                // A child kind cannot be a root, but its parent can: open that, and say which
                // row to press enter on.
                Reach::FromParent(parent) => {
                    self.status = Some(match self.open_root(parent) {
                        Open::Opened => format!(
                            "open {} from a row of {}: pick one and press enter",
                            kind.display, parent.display
                        ),
                        Open::Greyed(reason) => reason,
                    });
                }
                // Nothing to open: the reason is all this key can offer.
                Reach::Direct | Reach::NeedsParameter => self.status = Some(reason),
            },
            Entry::Kind { kind, reason: None } => {
                if let Open::Greyed(reason) = self.open_root(kind) {
                    self.status = Some(reason);
                }
            }
            Entry::Page(def) => {
                if let Open::Greyed(reason) = self.open_page(def) {
                    self.status = Some(reason);
                }
            }
            // Nothing to open: the note is all this key can offer, and saying it is the whole
            // reason the row is on the list at all.
            Entry::Missing(item) => {
                self.status = Some(match item.note {
                    Some(note) => format!("{}: {note}", item.label),
                    None => format!("{}: nothing to open", item.label),
                });
            }
        }
    }

    /// Recompute the palette for its input: `live`, `screen` and `palette` are borrowed as
    /// the separate fields they are, so the ranking can read the session and the context rows
    /// while the palette is borrowed mutably from the same `self`.
    ///
    /// `:ctx ` completes from the rows the Contexts screen shows, not from a fresh `list()`:
    /// that reads and parses the config file and probes every secret store for every context
    /// (on a keyring build, possibly with a dialog), and this runs on every keystroke. The
    /// rows are read at startup and by everything that changes them; a context added by
    /// another process is completed after the same `ctrl-r` that shows it on the screen.
    fn refresh_palette(&mut self) {
        // Nothing to refresh, nothing to build: the vocabulary walks the view and allocates, and
        // this runs on every palette keystroke.
        if self.palette.is_none() {
            return;
        }
        // Owned before the palette is borrowed mutably: `a.name` is `&'static str`, so this
        // borrows nothing of `self` past the end of the statement.
        let actions: Vec<&'static str> = self
            .view()
            .map(|v| {
                v.key
                    .kind
                    .action_target()
                    .actions
                    .iter()
                    .map(|a| a.name)
                    .collect()
            })
            .unwrap_or_default();
        let live = self.live.as_ref();
        let unavailable = move |kind: &Kind| unavailable_reason(&live?.session, kind);
        let contexts: Vec<&str> = self.screen.rows.iter().map(|r| r.name.as_str()).collect();
        let hidden: Vec<&str> = self.nav_hide.iter().map(String::as_str).collect();
        let vocab = palette::Vocabulary {
            unavailable: &unavailable,
            contexts,
            actions,
            hidden,
        };
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        palette.refresh(&vocab);
    }

    /// Re-read the rows; keep the selection on the same name where possible. A config file
    /// that cannot be read leaves the rows empty and puts the error in the message area:
    /// "no contexts" and "your config file is broken" are different answers, and only one of
    /// them is fixed by adding a context.
    pub(crate) fn reload_contexts(&mut self) {
        let keep = self.screen.current().map(|r| r.name.clone());
        // A reload answers whatever the last one said - a config file since fixed, a row since
        // removed - so it starts by dropping it. Callers with something to say say it after.
        self.screen.message = None;
        self.screen.list_error = None;
        self.screen.rows = match self.contexts.list() {
            Ok(rows) => rows,
            Err(e) => {
                self.screen.list_error = Some(format!("{e:#}"));
                Vec::new()
            }
        };
        self.screen.selected = keep
            .and_then(|name| self.screen.rows.iter().position(|r| r.name == name))
            .or_else(|| self.screen.rows.iter().position(|r| r.current))
            .unwrap_or(0);
    }

    /// Open the Contexts screen (from `:ctx`, or at startup). `row` preselects a name.
    pub(crate) fn show_contexts(&mut self, row: Option<&str>) {
        self.reload_contexts();
        if let Some(name) = row {
            self.screen.select(name);
        }
        self.mode = Mode::Contexts;
        self.dirty = true;
    }

    /// Spawn the connect and show what it is doing. The UI thread never waits on the network:
    /// the result comes back on a channel, like a poll. An open form or login field stays open
    /// until the result is in, so a rejected password costs the user nothing they typed.
    fn start_connect(&mut self, request: ConnectRequest, host: &str) {
        // `handle` runs inside the event loop's runtime; anything else calling it (a test with
        // no runtime, say) is a bug that must not take the terminal down with it. It reports
        // like any other failed connect, so it lands wherever the user is looking.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.connect_failed("no async runtime: cannot connect from here".into());
            return;
        };
        let future = self.contexts.connect(request);
        let tx = self.connect_tx.clone();
        runtime.spawn(async move {
            let _ = tx.send(future.await).await;
        });
        self.screen.pending = true;
        self.screen.message = Some(format!("connecting to {host}…"));
        self.mode = if self.screen.prompt.is_some() {
            Mode::Form
        } else {
            Mode::Contexts
        };
    }

    /// The outcome of a connect the screen started. Warnings (a password that could not be
    /// stored, say) stay in the message area behind the table; a failure goes back to whatever
    /// box asked for the connect, with what it says still on screen.
    pub fn connected(&mut self, result: anyhow::Result<Connected>) {
        self.screen.pending = false;
        self.dirty = true;
        match result {
            Ok(connected) => self.switch_to(connected),
            Err(e) => self.connect_failed(format!("{e:#}")),
        }
    }

    /// The session is real: it replaces whatever was there, and the boxes that asked for it
    /// close. What could not be done on the way is said next to it, not instead of it.
    fn switch_to(
        &mut self,
        Connected {
            session,
            warnings,
            cache_dir,
            restored,
        }: Connected,
    ) {
        // What was open before the switch, so `:ctx` comes back to the same screen.
        let was = match self.live.as_ref().and_then(|live| live.stack.last()) {
            Some(View::Table(view)) => Home::Kind(view.key.kind),
            Some(View::Page(page)) => Home::Page(page.def),
            None => self.home,
        };
        // `attach` writes the outgoing context's cache and clears the directory; this is the
        // incoming one, read before the connection that adopted its pins.
        self.attach(session);
        if let Some(dir) = cache_dir {
            self.enable_cache(dir, restored);
        }
        self.screen.prompt = None;
        self.reload_contexts();
        let mut notes = warnings;
        // The new Prism Central may not serve what the old one did.
        if let Open::Greyed(reason) = self.reopen(was) {
            notes.push(format!("{}: {reason}", was.label()));
            if let Open::Greyed(reason) = self.reopen(self.home) {
                // Nothing left to open: the screen that can pick another Prism Central is a
                // better place to sit than an empty table with no way back to one.
                notes.push(format!("{}: {reason}", self.home.label()));
                self.mode = Mode::Contexts;
            }
        }
        if !notes.is_empty() {
            self.screen.message = Some(notes.join("\n"));
            self.status = Some(notes.join("; "));
        }
    }

    /// Open whichever of the two a `Home` names, as the root view.
    fn reopen(&mut self, home: Home) -> Open {
        match home {
            Home::Kind(kind) => self.open_root(kind),
            Home::Page(def) => self.open_page(def),
        }
    }

    /// Back to the box that asked for the connect, so what was typed is still there to be
    /// corrected; the rows themselves when nothing was open.
    fn connect_failed(&mut self, error: String) {
        // Either way it replaces the `connecting to …` it answers.
        self.screen.message = None;
        match self.screen.prompt.as_mut() {
            Some(prompt) => {
                prompt.set_error(error);
                self.mode = Mode::Form;
            }
            None => {
                self.screen.message = Some(error);
                self.mode = Mode::Contexts;
            }
        }
    }

    /// Tests only: waits for the next connect result and applies it.
    ///
    /// The event loop cannot call this, which is why it is not on its path: it selects over the
    /// connect channel alongside the poll channel and the terminal's events, and a method that
    /// borrows the whole app cannot sit in one arm of that `select!` while another arm borrows
    /// the poll channel out of the same app.
    #[cfg(feature = "testing")]
    pub async fn await_connect(&mut self) {
        if let Some(result) = self.connect_rx.recv().await {
            self.connected(result);
        }
    }

    /// `:ctx`, with or without a name.
    pub(crate) fn ctx_command(&mut self, argument: Option<String>) {
        let Some(name) = argument else {
            self.show_contexts(None);
            return;
        };
        self.reload_contexts();
        let Some(row) = self.screen.rows.iter().find(|r| r.name == name).cloned() else {
            // Nothing opened, so nothing to go back from: the palette already restored the
            // mode it was opened over.
            self.status = Some(format!("no context named {name}"));
            return;
        };
        self.screen.select(&name);
        if row.has_password {
            self.start_connect(screen::request(&name, None), &row.host);
        } else {
            // Nothing to connect with: the screen asks for the password rather than failing,
            // and says why inside the box, which covers the rows the reason would go under.
            let why = nutsh_core::contexts::no_stored_password(&name);
            self.screen.open_login(name, Some(why));
            self.mode = Mode::Form;
        }
    }

    /// `:skin NAME` applies straight away; bare `:skin` opens the list.
    fn skin_command(&mut self, argument: Option<String>) {
        match argument {
            Some(name) => self.apply_skin(&name),
            None => {
                self.skins = Some(Skins::open());
                self.skins_from = self.mode;
                self.mode = Mode::Skins;
            }
        }
    }

    /// Apply live, then persist. A failed write is a status line, not a refusal: the skin is
    /// already on screen and telling the user it did not happen would be a lie.
    fn apply_skin(&mut self, name: &str) {
        self.status = Some(match crate::theme::apply_named(name) {
            Err(reason) => reason,
            // The canonical name: what the list marks, and the spelling the config gets.
            Ok(name) => match self.contexts.set_skin(name) {
                Ok(()) => format!("skin {name}"),
                Err(e) => format!("skin {name}; not saved: {e:#}"),
            },
        });
        self.dirty = true;
    }

    /// `:header` walks the three settings and writes the one it lands on to the config file,
    /// the way `:mouse` writes its toggle. A cycle rather than an argument because there are
    /// three settings and two of them are the interesting ones: `auto` is where a session
    /// starts, and the two overrides are one keystroke apart from it in either direction.
    ///
    /// A failed write is a red status line, not a refusal: the header is already drawn the new
    /// way, and saying so twice would be the only honest alternative to saying it once.
    fn header_command(&mut self) {
        self.header = match self.header {
            Header::Auto => Header::Compact,
            Header::Compact => Header::Full,
            Header::Full => Header::Auto,
        };
        let state = match self.header {
            Header::Auto => "auto",
            Header::Compact => "compact",
            Header::Full => "full",
        };
        self.status = Some(match self.contexts.set_header(self.header) {
            Ok(()) => format!("header {state}"),
            Err(e) => format!("header {state} for this session: {e}"),
        });
    }

    /// `:mouse` toggles capture and writes the answer to the config file, the way `:skin` writes
    /// `[skin].name`. A failed write is a red status line, not a refusal: the mouse is already
    /// on or off and telling the user it did not happen would be a lie.
    fn mouse_command(&mut self) {
        self.mouse_on = !self.mouse_on;
        let state = if self.mouse_on { "on" } else { "off" };
        self.status = Some(match self.contexts.set_mouse(self.mouse_on) {
            Ok(()) => format!("mouse {state}"),
            Err(e) => format!("mouse {state}; not saved: {e:#}"),
        });
        self.dirty = true;
    }

    /// `:log [level]` moves what this session writes down and writes the answer to the config
    /// file, the way `:mouse` and `:header` write theirs.
    ///
    /// Bare it is a switch between `off` and `debug`, and not a walk through six rungs the way
    /// `:header` walks three: this is the command somebody reaches for while something is
    /// already going wrong, and `error` and `warn` are not what they are reaching for. A level
    /// named outright still sets exactly that one.
    ///
    /// The level moves first and is saved second, and a save that failed is a status line
    /// rather than a refusal - the lines are already being written, and saying they are not
    /// would be the lie. A run that installed no subscriber at all says so instead of
    /// pretending: there is nothing listening for it to have changed.
    fn log_command(&mut self, args: &[String]) {
        let now = nutsh_core::log::state();
        let next = match args.first() {
            Some(word) => match nutsh_core::contexts::LogLevel::parse(word) {
                Some(level) => level,
                None => {
                    self.status = Some(format!(
                        "log: {word} is not a level; try off, error, warn, info, debug or trace"
                    ));
                    return;
                }
            },
            None if now.is_some_and(|l| l.on()) => nutsh_core::contexts::LogLevel::Off,
            None => nutsh_core::contexts::LogLevel::Debug,
        };
        self.status = Some(match nutsh_core::log::set(next) {
            Err(e) => format!("log {next}: {e:#}"),
            Ok(state) => {
                let where_to = match (&state.path, state.on()) {
                    (Some(path), true) => format!(" to {}", path.display()),
                    _ => String::new(),
                };
                match self.contexts.set_log(next) {
                    Ok(()) => format!("log {next}{where_to}"),
                    Err(e) => format!("log {next}{where_to}; not saved: {e:#}"),
                }
            }
        });
    }

    fn handle_skins(&mut self, key: Key) {
        let Some(list) = self.skins.as_mut() else {
            return;
        };
        match list.key(key) {
            skins::Action::Moved | skins::Action::Ignored => {}
            skins::Action::Cancelled => {
                self.skins = None;
                self.mode = self.skins_from;
            }
            skins::Action::Chose(name) => {
                self.skins = None;
                self.mode = self.skins_from;
                self.apply_skin(name);
            }
        }
        // Opened from the settings screen, so come back to it with the skin row showing the
        // skin that was just applied rather than the one that was showing when it opened.
        if self.mode == Mode::Settings {
            self.reopen_settings();
        }
    }

    /// The settings screen, over whatever is on screen. Its own mode, like the skin list, with
    /// the mode behind it remembered so `esc` goes back where it came from.
    pub(crate) fn show_settings(&mut self) {
        // The three reads finish before the field is assigned, so `self` is never borrowed
        // immutably and mutably at once.
        let rows = self.contexts.settings();
        let skin = crate::theme::current_name();
        let refresh = self.refresh_rows();
        let screen = crate::settings::Settings::open(&rows, &skin, &refresh, &self.nav_hide);
        self.settings = Some(screen);
        self.settings_from = self.mode;
        self.mode = Mode::Settings;
        self.dirty = true;
    }

    /// The schedule rows: `[refresh] default` always, and the view in front of the user when
    /// there is one. The kind's row carries its display name, because `refresh` alone over two
    /// rows would say nothing about which of them is which.
    fn refresh_rows(&self) -> Vec<crate::settings::Row> {
        use nutsh_core::contexts::{SettingId, Source};
        let global = self.refresh_cfg.default;
        let ages = self.poll_ages();
        let mut out = vec![crate::settings::Row::Setting {
            id: SettingId::Refresh,
            label: "every kind".to_string(),
            value: global.map_or_else(|| "auto".to_string(), nutsh_core::refresh::show),
            source: if global.is_some() {
                Source::File.label()
            } else {
                Source::Default.label()
            },
            fixed: None,
            kind: None,
            age: None,
        }];

        // By namespace, because two hundred and thirty-two rows of which two hundred and
        // twenty-six say the same thing is a list nobody reads. A namespace opens onto its own
        // kinds, and the ones somebody has set are counted on its row whether it is open or not.
        for ns in nutsh_catalog::NAMESPACES {
            let kinds: Vec<&'static Kind> = nutsh_catalog::KINDS
                .iter()
                .filter(|k| k.namespace == ns.name)
                .collect();
            if kinds.is_empty() {
                continue;
            }
            let own = self.refresh_cfg.namespaces.get(ns.name).copied();
            let custom = kinds
                .iter()
                .filter(|k| self.refresh_cfg.kinds.contains_key(k.id))
                .count();
            let open = self.settings_open.contains(ns.name);
            out.push(crate::settings::Row::Namespace {
                name: ns.name,
                value: own.map_or_else(|| "-".to_string(), nutsh_core::refresh::show),
                source: own.map_or_else(
                    || Source::Default.label(),
                    |_| Source::Namespace(ns.name).label(),
                ),
                open,
                custom,
            });
            if !open {
                continue;
            }
            let mut rows: Vec<(Option<std::time::Duration>, Source, &'static Kind)> = kinds
                .into_iter()
                .map(|k| {
                    let (every, source) = self.refresh_of(k);
                    (every, source, k)
                })
                .collect();
            // Soonest first inside the namespace: `off` is not a very long interval, it is no
            // schedule, so it sorts last.
            rows.sort_by(|a, b| {
                let key = |d: &Option<std::time::Duration>| d.map_or(u64::MAX, |d| d.as_secs());
                key(&a.0).cmp(&key(&b.0)).then(a.2.display.cmp(b.2.display))
            });
            out.extend(
                rows.into_iter()
                    .map(|(every, source, k)| crate::settings::Row::Setting {
                        id: SettingId::RefreshKind,
                        label: format!("  {}", k.display),
                        value: every.map_or_else(
                            || "off".to_string(),
                            |d| nutsh_core::cell::span(d.as_secs()),
                        ),
                        source: source.label(),
                        fixed: None,
                        kind: Some(k.id),
                        age: ages.get(k.id).cloned(),
                    }),
            );
        }
        out
    }

    /// How long ago each kind's table last completed a cycle, rendered. A kind with no table
    /// open is absent: it has not refreshed, which is not the same as having refreshed long ago.
    fn poll_ages(&self) -> std::collections::HashMap<&'static str, String> {
        let now = std::time::Instant::now();
        let mut out = std::collections::HashMap::new();
        let Some(live) = self.live.as_ref() else {
            return out;
        };
        for (key, table) in live.store.tables() {
            let Some(at) = table.last_poll else { continue };
            let secs = now.saturating_duration_since(at).as_secs();
            out.entry(key.kind.id)
                .or_insert_with(|| nutsh_core::cell::span(secs));
        }
        out
    }

    fn close_settings(&mut self) {
        self.settings = None;
        self.mode = self.settings_from;
        self.dirty = true;
    }

    /// Rebuild the rows from the seam, keeping the cursor where it was: a toggle changes a
    /// value and a `d` changes the list, and both have to be visible without closing the screen.
    ///
    /// `settings_from` is restored around the rebuild - `show_settings` records the mode it was
    /// called from, and here that mode is `Settings` itself, which would make `esc` reopen the
    /// screen it just closed.
    fn reopen_settings(&mut self) {
        let selected = self.settings.as_ref().map_or(0, |s| s.selected);
        let from = self.settings_from;
        self.show_settings();
        self.settings_from = from;
        if let Some(screen) = self.settings.as_mut() {
            screen.selected = selected.min(screen.rows.len().saturating_sub(1));
        }
    }

    fn handle_settings(&mut self, key: Key) {
        let Some(screen) = self.settings.as_mut() else {
            return;
        };
        match screen.key(key) {
            crate::settings::Action::Moved | crate::settings::Action::Ignored => {}
            crate::settings::Action::Cancelled => self.close_settings(),
            crate::settings::Action::Fixed(why) => self.status = Some(why),
            crate::settings::Action::Skins => {
                // Over the settings screen, not instead of it: `esc` in the skin list comes
                // back here, which is where the user was.
                self.skins = Some(Skins::open());
                self.skins_from = Mode::Settings;
                self.mode = Mode::Skins;
            }
            crate::settings::Action::Add => {
                self.close_settings();
                // The verb the palette already completes: one ranking, one writer.
                self.open_palette_with("hide ");
            }
            crate::settings::Action::Remove(name) => {
                self.nav_command(Some(name), false);
                self.reopen_settings();
            }
            crate::settings::Action::RefreshAll => {
                let n = self
                    .live
                    .as_mut()
                    .map_or(0, |live| live.scheduler.refresh_everything());
                self.status = Some(match n {
                    0 => "nothing subscribed to refresh".to_string(),
                    1 => "refreshing 1 view".to_string(),
                    n => format!("refreshing {n} views"),
                });
            }
            crate::settings::Action::Expand(ns) => {
                if !self.settings_open.remove(ns) {
                    self.settings_open.insert(ns);
                }
                self.reopen_settings();
            }
            crate::settings::Action::CycledNamespace(ns) => {
                // The rung after whatever the namespace itself says, not after what its kinds
                // resolve to: those differ from each other, and a step has to start somewhere.
                let now = self
                    .refresh_cfg
                    .namespaces
                    .get(ns)
                    .copied()
                    .or(self.refresh_cfg.default);
                let next = nutsh_core::refresh::step_from(now);
                self.status = Some(
                    match self
                        .contexts
                        .set_interval(nutsh_core::contexts::Schedule::Namespace(ns), next)
                    {
                        Ok(()) => {
                            self.refresh_cfg = self.contexts.refresh();
                            format!("{ns}: {}", nutsh_core::refresh::show(next))
                        }
                        Err(e) => format!("refresh not saved: {e:#}"),
                    },
                );
                if let Some(live) = self.live.as_ref() {
                    for k in nutsh_catalog::KINDS.iter().filter(|k| k.namespace == ns) {
                        live.scheduler.set_interval_kind(k.id, self.refresh_of(k).0);
                    }
                }
                self.reopen_settings();
            }
            crate::settings::Action::Cycled(id, row_kind) => {
                let _ = id;
                let kind = row_kind.and_then(nutsh_catalog::kind);
                let next = match kind {
                    // A kind's row steps from the length of time it is actually polling at, so
                    // a 3 s kind does not spend a press on the 5 s it is nearly already at.
                    Some(k) => nutsh_core::refresh::step(self.refresh_of(k).0),
                    // The global row names a rung and stands for no kind in particular, so it
                    // walks the ladder by position instead.
                    None => nutsh_core::refresh::step_from(self.refresh_cfg.default),
                };
                // The file is where this value lives now, so a `ctrl-t` override for the same
                // kind goes: the source column has to say `config file`, not `this session`.
                if let Some(k) = kind {
                    self.refresh_session.remove(k.id);
                }
                self.status = Some(
                    match self.contexts.set_interval(
                        kind.map_or(nutsh_core::contexts::Schedule::Everything, |k| {
                            nutsh_core::contexts::Schedule::Kind(k.id)
                        }),
                        next,
                    ) {
                        Ok(()) => {
                            self.refresh_cfg = self.contexts.refresh();
                            format!("refresh: {}", nutsh_core::refresh::show(next))
                        }
                        Err(e) => format!("refresh not saved: {e:#}"),
                    },
                );
                if let Some(k) = kind
                    && let Some(live) = self.live.as_ref()
                {
                    live.scheduler.set_interval_kind(k.id, self.refresh_of(k).0);
                }
                self.reopen_settings();
            }
            crate::settings::Action::Cycle(id) => {
                match id {
                    nutsh_core::contexts::SettingId::Mouse => self.mouse_command(),
                    nutsh_core::contexts::SettingId::Log => self.log_command(&[]),
                    _ => self.header_command(),
                }
                self.reopen_settings();
            }
            crate::settings::Action::Toggled(id, on) => {
                self.status = Some(match self.contexts.set_flag(id, on) {
                    Ok(()) => format!("{} {}", id.label(), if on { "on" } else { "off" }),
                    Err(e) => format!("{} not saved: {e:#}", id.label()),
                });
                // The menu is downstream of one of these switches, so recompute it here rather
                // than making every caller remember which.
                if id == nutsh_core::contexts::SettingId::HideUnserved {
                    self.nav_hide_unserved = on;
                    self.apply_nav();
                }
                self.reopen_settings();
            }
        }
    }

    /// The rows: the screen owns the keys, the app owns what they need to reach.
    fn handle_contexts(&mut self, key: Key) {
        let action = self.screen.key(key);
        self.screen_action(action);
    }

    /// The add form and the login field, likewise.
    fn handle_form(&mut self, key: Key) {
        let action = self.screen.form_key(key);
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

    /// The context is gone once `remove` returns `Ok`, so a secret store that could not be
    /// reached is reported next to the removal rather than instead of it.
    fn remove_context(&mut self, name: &str) {
        let message = match self.contexts.remove(name) {
            Ok(warnings) if warnings.is_empty() => format!("removed {name}"),
            Ok(warnings) => format!("removed {name}; {}", warnings.join("; ")),
            Err(e) => format!("{e:#}"),
        };
        let was = self.screen.selected;
        self.reload_contexts();
        // The row under the cursor is the one that went: staying where it was leaves the
        // cursor on its neighbour, which is where the eye already is.
        self.screen.selected = was.min(self.screen.rows.len().saturating_sub(1));
        self.screen.message = Some(message);
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

impl App {
    /// A pointer event. Sets `dirty` only when something moved: the wheel over a list already at
    /// its end changes nothing, and redrawing for it would burn a frame.
    pub fn mouse(&mut self, m: Mouse) {
        // A pointer is somebody being there, exactly as a key is: a session driven by the mouse
        // alone must not drift into the idle pause while it is being used.
        self.woke();
        let landed = self.landed(m);
        if self.act(landed) {
            self.dirty = true;
        }
    }

    /// Whether the event loop should capture the mouse.
    pub fn mouse_enabled(&self) -> bool {
        self.mouse_on
    }

    /// Where `m` fell, read off the hit map and nothing else.
    fn landed(&self, m: Mouse) -> Landed {
        use crate::mouse::{MouseKind, contains};
        let hits = self.hits.borrow();
        // A modal owns the pointer while it is up. Everything outside its list is inert: a
        // stray click must not throw away a half-typed `:` command, and it must not reach the
        // table underneath either. `Mode::Detail`, `Mode::Help`, `Mode::Form` and
        // `Mode::Contexts` record no modal list at all and so answer `Nothing` for every event.
        if self.mode != Mode::Table {
            return match (m.kind, hits.modal.as_ref()) {
                (MouseKind::Click, Some(list)) => {
                    list.index_at(m).map_or(Landed::Nothing, Landed::Modal)
                }
                (MouseKind::WheelDown, Some(list)) if contains(list.rows, m) => {
                    Landed::ModalWheel(true)
                }
                (MouseKind::WheelUp, Some(list)) if contains(list.rows, m) => {
                    Landed::ModalWheel(false)
                }
                _ => Landed::Nothing,
            };
        }
        if let Some(list) = hits.sidebar.as_ref()
            && contains(list.rows, m)
        {
            return match m.kind {
                MouseKind::Click => list.index_at(m).map_or(Landed::Nothing, Landed::Menu),
                MouseKind::WheelDown => Landed::MenuWheel(true),
                MouseKind::WheelUp => Landed::MenuWheel(false),
                _ => Landed::Nothing,
            };
        }
        for t in hits.tables.iter() {
            if !contains(t.block, m) {
                continue;
            }
            let target = t.pane.map_or(Target::Table, Target::Pane);
            return match m.kind {
                MouseKind::Click if contains(t.header, m) => match t.column_at(m) {
                    Some(column) => Landed::Header { target, column },
                    None => Landed::Block { target },
                },
                MouseKind::Click => match t.index_at(m) {
                    Some(row) => Landed::Row { target, row },
                    // A pane's border, its title, its empty area, or the space below its last
                    // row: focus it and select nothing.
                    None => Landed::Block { target },
                },
                MouseKind::WheelDown => Landed::Wheel { target, down: true },
                MouseKind::WheelUp => Landed::Wheel {
                    target,
                    down: false,
                },
                MouseKind::WheelLeft if target == Target::Table => Landed::Columns { right: false },
                MouseKind::WheelRight if target == Target::Table => Landed::Columns { right: true },
                MouseKind::WheelLeft | MouseKind::WheelRight => Landed::Nothing,
            };
        }
        // The header box, the stats block, the prompt line, the status line.
        Landed::Nothing
    }

    /// Act on it, and say whether anything moved.
    fn act(&mut self, landed: Landed) -> bool {
        match landed {
            Landed::Nothing => false,
            Landed::Modal(i) => self.select_in_modal(i),
            Landed::ModalWheel(down) => {
                let selected = self.modal_selected();
                let next = step(selected, down, WHEEL_ROWS, self.modal_len());
                self.select_in_modal(next)
            }
            Landed::Menu(i) => {
                self.focus_sidebar();
                // A click on the caption between the curated menu and the namespaces selects
                // nothing and opens nothing; the focus still moves, so the click is not lost.
                if self.sidebar.select(i, false) {
                    // What `enter` does on that row: open an item, toggle a group.
                    let action = self.sidebar.key(Key::Enter);
                    self.sidebar_action(action);
                }
                true
            }
            Landed::MenuWheel(down) => {
                let next = step(
                    self.sidebar.selected,
                    down,
                    WHEEL_ROWS,
                    self.sidebar.rows().len(),
                );
                let moved = next != self.sidebar.selected;
                self.sidebar.select(next, true);
                moved
            }
            Landed::Row { target, row } => {
                let focused = self.focus_table(target);
                // Not short-circuited: the selection moves whether or not the focus did.
                self.set_selection(target, row) || focused
            }
            Landed::Header { target, column } => {
                self.focus_table(target);
                // A pane sorts nothing: it has no `S` either.
                if target != Target::Table {
                    return true;
                }
                self.sort_by(column);
                true
            }
            Landed::Block { target } => self.focus_table(target),
            Landed::Wheel { target, down } => {
                // Deliberately no `focus_table`: scrolling a pane you are only glancing at
                // should not steal the keyboard.
                let selected = self.selection(target);
                let next = step(selected, down, WHEEL_ROWS, self.rows_len(target));
                self.set_selection(target, next)
            }
            Landed::Columns { right } => {
                self.handle_table(if right { Key::Right } else { Key::Left });
                true
            }
        }
    }

    /// Focus the body, and on a page the pane that was clicked. Says whether either moved.
    fn focus_table(&mut self, target: Target) -> bool {
        let mut moved = self.focus != Focus::Body;
        self.focus = Focus::Body;
        if let Target::Pane(i) = target
            && let Some(live) = self.live.as_mut()
            && let Some(page) = live.page_mut()
            && page.focus != i
            && i < page.panes.len()
        {
            page.focus = i;
            moved = true;
        }
        moved
    }

    /// The cursor of the table view, or of the named pane on a page.
    fn selection(&self, target: Target) -> usize {
        match target {
            Target::Pane(i) => self
                .page()
                .and_then(|page| page.panes.get(i))
                .map_or(0, |pane| pane.selected),
            Target::Table => self
                .live
                .as_ref()
                .and_then(Live::table)
                .map_or(0, |view| view.selected),
        }
    }

    /// How many rows it has, which is what a wheel is clamped to.
    fn rows_len(&self, target: Target) -> usize {
        let Some(live) = self.live.as_ref() else {
            return 0;
        };
        let key = match target {
            Target::Pane(i) => match live.page().and_then(|page| page.panes.get(i)) {
                Some(pane) => &pane.key,
                None => return 0,
            },
            Target::Table => match live.table() {
                Some(view) => &view.key,
                None => return 0,
            },
        };
        live.store.table(key).rows.len()
    }

    /// Move it, and say whether it moved.
    fn set_selection(&mut self, target: Target, row: usize) -> bool {
        let Some(live) = self.live.as_mut() else {
            return false;
        };
        match target {
            Target::Pane(i) => match live.page_mut().and_then(|page| page.panes.get_mut(i)) {
                Some(pane) if pane.selected != row => {
                    pane.selected = row;
                    true
                }
                _ => false,
            },
            Target::Table => match live.table_mut() {
                Some(view) if view.selected != row => {
                    view.selected = row;
                    true
                }
                _ => false,
            },
        }
    }

    /// The open modal's cursor, its length, and where to move it. All three dispatch on the
    /// mode rather than on which `Option` is `Some`, because the palette can be open behind the
    /// skin list and only one of them is taking keys.
    fn modal_selected(&self) -> usize {
        match self.mode {
            Mode::Command => self.palette.as_ref().map_or(0, |p| p.selected),
            Mode::Picker => self.picker.as_ref().map_or(0, |p| p.selected),
            Mode::Skins => self.skins.as_ref().map_or(0, |s| s.selected),
            _ => 0,
        }
    }

    fn modal_len(&self) -> usize {
        match self.mode {
            Mode::Command => self.palette.as_ref().map_or(0, |p| p.entries.len()),
            Mode::Picker => self.picker.as_ref().map_or(0, |p| p.entries.len()),
            Mode::Skins => crate::theme::BUILTIN_NAMES.len(),
            _ => 0,
        }
    }

    fn select_in_modal(&mut self, i: usize) -> bool {
        if i >= self.modal_len() || i == self.modal_selected() {
            return false;
        }
        match self.mode {
            Mode::Command => {
                if let Some(p) = self.palette.as_mut() {
                    p.selected = i;
                }
            }
            // Through `Picker::select`, which refuses a caption: a click on the rule between
            // the related kinds and the actions selects nothing.
            Mode::Picker => {
                if !self.picker.as_mut().is_some_and(|p| p.select(i)) {
                    return false;
                }
            }
            Mode::Skins => {
                if let Some(s) = self.skins.as_mut() {
                    s.selected = i;
                }
            }
            _ => return false,
        }
        true
    }

    /// Sort by the column at `all_index` in the view's full column list: ascending if it was not
    /// the sort column, otherwise flipped. `S` keeps cycling through the columns as it does; a
    /// click names one.
    pub(crate) fn sort_by(&mut self, all_index: usize) {
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.table_mut()
        {
            view.sort = match view.sort {
                Some((col, reverse)) if col == all_index => Some((all_index, !reverse)),
                _ => Some((all_index, false)),
            };
        }
    }

    /// Tests only: the hit map the last `frame` recorded.
    #[cfg(feature = "testing")]
    pub fn hits_for_test(&self) -> std::cell::Ref<'_, crate::mouse::Hits> {
        self.hits.borrow()
    }

    /// Tests only: the current table's sort, as `(index into the full column list, reversed)`.
    #[cfg(feature = "testing")]
    pub fn sort_for_test(&self) -> Option<(usize, bool)> {
        self.live.as_ref()?.table()?.sort
    }

    /// Tests only: the palette's selected entry.
    #[cfg(feature = "testing")]
    pub fn palette_selected_for_test(&self) -> usize {
        self.palette.as_ref().map_or(0, |p| p.selected)
    }

    /// Tests only: which pane of the open page has the cursor.
    #[cfg(feature = "testing")]
    pub fn focused_pane_for_test(&self) -> Option<usize> {
        Some(self.page()?.focus)
    }

    /// Tests only: what the event loop does after it draws.
    #[cfg(feature = "testing")]
    pub fn clear_dirty_for_test(&mut self) {
        self.dirty = false;
    }
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
