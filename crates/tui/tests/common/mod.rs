#![allow(dead_code)]

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nutsh_core::contexts::{
    ConnectRequest, Connected, ContextRow, Contexts, Flags, Setting, SettingId,
};
use nutsh_core::guardrails::Guardrails;
use nutsh_core::session::{Scope, Session};
use nutsh_mockpc::MockPc;
use nutsh_prism::Profile;
use nutsh_tui::App;
use nutsh_tui::app::Config;
use ratatui::buffer::Buffer;
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

/// 2026-09-05T10:00:00Z, so ages in snapshots are stable.
pub fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_788_602_400)
}

pub fn profile(pc: &MockPc) -> Profile {
    Profile {
        host: pc.host(),
        port: pc.port(),
        username: "admin".into(),
        verify_tls: true,
        ca_bundle: None,
        plain_http: true,
    }
}

pub async fn session(pc: &MockPc) -> Session {
    nutsh_core::session::connect(&profile(pc), "secret", Scope::default(), None)
        .await
        .unwrap()
}

/// A connected app whose cache lives under `state`, read before connecting and written on
/// demand - the shape `src/tui.rs` builds for a real run.
pub async fn app_with_cache(pc: &MockPc, state: &std::path::Path) -> App {
    let root = state.join("nutsh").join("cache");
    let profile = profile(pc);
    let identity = nutsh_core::cache::Identity {
        host: profile.host.clone(),
        port: profile.port,
        username: profile.username.clone(),
        domain_manager: None,
        pc_version: None,
    };
    let key = nutsh_core::cache::slug(&format!(
        "{}@{}:{}",
        profile.username, profile.host, profile.port
    ))
    .expect("a key");
    let dir = root.join(key);
    let restored = nutsh_core::cache::read(
        &dir,
        &identity,
        nutsh_core::cache::now_secs(),
        nutsh_core::cache::MAX_AGE,
    );
    let session =
        nutsh_core::session::connect(&profile, "secret", Scope::default(), restored.as_ref())
            .await
            .unwrap();
    let mut app = App::open(session, Box::new(NoContexts), "vm", config()).unwrap();
    app.enable_cache(dir, restored);
    app
}

/// Contexts with scripted rows and no ability to connect; enough for table and palette tests.
pub struct NoContexts;

impl Contexts for NoContexts {
    fn list(&self) -> anyhow::Result<Vec<ContextRow>> {
        Ok(Vec::new())
    }
    fn connect(
        &self,
        _req: ConnectRequest,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<Connected>> {
        Box::pin(async { anyhow::bail!("no contexts in this test") })
    }
    fn remove(&self, _name: &str) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("no contexts in this test")
    }
}

pub fn config() -> Config {
    Config {
        now: now(),
        config_path: None,
        guardrails: Guardrails::default(),
        snapshot: false,
        mouse: true,
        // **Pinned, not defaulted.** These suites choose a frame height to make the thing under
        // test fit - fourteen rows to drop a pane, twelve to leave no room for a row - not to
        // model a terminal somebody owns, and almost all of them are under the height at which
        // `Header::Auto` folds. Letting them fold would re-record fifteen snapshots to test the
        // same things through a different header, and would leave the full header with no
        // coverage at all. `tests/header.rs` is where the two heights and the automatic choice
        // are tested, and the CLI's `tests/snapshot.rs` exercises `auto` end to end.
        header: nutsh_tui::app::Header::Full,
    }
}

/// A config whose guardrails come from a `[[guardrails]]` fragment, so a test can write the
/// rule it means in the file's own syntax.
pub fn config_with_guardrails(toml_text: &str) -> Config {
    #[derive(serde::Deserialize)]
    struct Wrapper {
        #[serde(default)]
        guardrails: Vec<nutsh_config::RuleSpec>,
    }
    let specs = toml::from_str::<Wrapper>(toml_text)
        .expect("valid guardrails")
        .guardrails;
    Config {
        now: now(),
        config_path: None,
        guardrails: Guardrails::from_config(false, &specs).expect("valid rules"),
        snapshot: false,
        mouse: true,
        // As `config()` pins it, and for the reason spelled out there.
        header: nutsh_tui::app::Header::Full,
    }
}

/// Runs the app's poll channel until the first `Complete` or `Error` for the current table.
pub async fn settle(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(5), app.settle_once())
        .await
        .expect("the first poll settles");
}

/// Drains the poll channel until the first `Acted`.
pub async fn settle_acted(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(5), app.settle_acted())
        .await
        .expect("the action reports back");
}

/// Runs the app's poll channel until every pane of the open page has settled.
pub async fn settle_page(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(10), app.settle_page_once())
        .await
        .expect("every pane settles");
}

/// Runs the app's poll channel until the DR sampler's first cycle lands.
pub async fn settle_sample(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(20), app.settle_sample_once())
        .await
        .expect("the sampler reports");
}

/// Runs the app's poll channel until the stats poller's first cycle lands.
pub async fn settle_stats(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(20), app.settle_stats_once())
        .await
        .expect("the stats poller reports");
}

/// Runs the app's poll channel until every kind the open search asked has answered.
pub async fn settle_search(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(20), app.settle_search())
        .await
        .expect("every kind the search asked answers");
}

/// Runs the app's poll channel until the name warm-up's first cycle lands.
pub async fn settle_names(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(20), app.settle_names_once())
        .await
        .expect("the warm-up reports");
}

/// Waits for the connect the app started and applies its result.
pub async fn finish_connect(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(5), app.await_connect())
        .await
        .expect("the connect finishes");
}

/// Snapshot settings: everything a frame shows that is not the same on every run, which is
/// the mock's ephemeral port; it becomes `PORT`.
pub fn settings(pc: &MockPc) -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.add_filter(
        &format!("{}:{}", regex_escape(&pc.host()), pc.port()),
        "127.0.0.1:PORT",
    );
    s.add_filter(r"127\.0\.0\.1:\d+", "127.0.0.1:PORT");
    // The meter is a timing value. The leading spaces are inside the match because the cell is
    // right-aligned: `2.4 req/s` and `12.4 req/s` pad differently, and a snapshot that
    // recorded the padding would be non-deterministic in a way no diff explains.
    s.add_filter(r" *\d+\.\d req/s( · (\d+%|-) cached)?", "[meter]");
    // And the cached badge's age, which moves with the clock for the same reason. The whole
    // alphabet `nutsh_core::cell::age` spells, `y` included.
    s.add_filter(r"◌ cached \d+[smhdy]", "[cached]");
    s
}

fn regex_escape(s: &str) -> String {
    s.replace('.', r"\.")
}

/// Mocha at truecolour, with the recorded name pinned to `catppuccin-mocha` too, before any
/// style assertion, and the lock that keeps one test's `:skin` out of another's frame: the
/// active palette, its name and the depth are process globals, `cargo test` runs a binary's
/// tests on several threads, and a test binary never calls `theme::install`.
/// Every test that reads or changes either holds the guard until it returns. Take it after the
/// `.await`s that build the app - nothing reads the palette until a frame is drawn - so no
/// guard is held across an await.
pub fn pin_skin() -> std::sync::MutexGuard<'static, ()> {
    static SKIN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = SKIN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    nutsh_tui::theme::set_depth(nutsh_tui::theme::Depth::Truecolor);
    // `apply_named`, not `init`: it sets the palette and the recorded name in one call, so a
    // test that switched to `gruvbox-dark` cannot leave its name in the next test's `:skin` list.
    nutsh_tui::theme::apply_named("catppuccin-mocha").expect("mocha is a built-in");
    guard
}

/// The style of the first cell of the first run of `needle` in `buf`.
///
/// It finds text rather than coordinates, so a layout change cannot silently move a colour
/// assertion onto a different cell - it fails instead, naming the frame.
pub fn style_of(buf: &Buffer, needle: &str) -> Style {
    style_of_within(buf, 0..buf.area.width, needle)
}

/// `style_of` over a band of columns: the menu and the body sit side by side and both draw a
/// table's name and its count, so a needle has to say which pane it means.
pub fn style_of_within(buf: &Buffer, cols: std::ops::Range<u16>, needle: &str) -> Style {
    let Some((x, y)) = find_cell(buf, cols, needle) else {
        panic!("{needle:?} is not in the frame:\n{}", text_of(buf));
    };
    let cell = buf.cell((x, y)).expect("inside the buffer");
    Style::default()
        .fg(cell.fg)
        .bg(cell.bg)
        .add_modifier(cell.modifier)
}

/// The heading every table view anchors on: `table::rule` pins column 0, and `columns` never
/// drops it, so it is the one heading a frame with a table in it always has.
const ANCHOR_HEADING: &str = "NAME";

/// The columns from a table's `heading` rightwards, so a needle can say which column it means
/// without a magic number: the widths depend on what the rows hold.
///
/// The heading is looked for on the table's heading row - the row [`ANCHOR_HEADING`] is on -
/// rather than on the first row of the frame that happens to contain the word. A hint in the
/// header box or a counter in the stats block spelling the same thing would otherwise move the
/// band, and every assertion made through it, onto another part of the frame.
pub fn column_band(buf: &Buffer, heading: &str) -> std::ops::Range<u16> {
    let all = 0..buf.area.width;
    let Some((_, y)) = find_cell(buf, all.clone(), ANCHOR_HEADING) else {
        panic!(
            "no {ANCHOR_HEADING} heading in the frame:\n{}",
            text_of(buf)
        );
    };
    let row = row_text(buf, all, y);
    let Some(byte) = row.find(heading) else {
        panic!(
            "no {heading:?} heading on the table's heading row:\n{}",
            text_of(buf)
        );
    };
    cell_x(&row, byte, 0)..buf.area.width
}

/// The (x, y) of the first cell of the first run of `needle` within `cols`.
fn find_cell(buf: &Buffer, cols: std::ops::Range<u16>, needle: &str) -> Option<(u16, u16)> {
    (0..buf.area.height).find_map(|y| {
        let row = row_text(buf, cols.clone(), y);
        row.find(needle)
            .map(|byte| (cell_x(&row, byte, cols.start), y))
    })
}

/// One row of `buf`, over a band of columns, as the text it draws.
///
/// Public because a test that asserts *where* something is drawn - the status line's
/// right-hand block, whose padding the `[meter]` filter deliberately keeps out of the
/// snapshots - reads a row by column rather than by needle.
pub fn row_text(buf: &Buffer, cols: std::ops::Range<u16>, y: u16) -> String {
    cols.map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect()
}

/// Where a byte offset into a [`row_text`] that started at `start` falls in the buffer. Cells,
/// not characters: the box-drawing and gutter glyphs before it are one cell each, but a CJK
/// name in a column to its left is two.
fn cell_x(row: &str, byte: usize, start: u16) -> u16 {
    start + u16::try_from(row[..byte].width()).unwrap_or(u16::MAX)
}

/// Drawn lines as the terminal would print them, styles dropped: what a text snapshot sees,
/// for a test that measures the lines a pane would draw rather than a whole frame.
pub fn text_of_lines(lines: &[ratatui::text::Line<'static>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                + "\n"
        })
        .collect()
}

/// A buffer as the text `App::snapshot` would print, for a failure message.
pub fn text_of(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let line: String = (0..buf.area.width)
            .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// A seam over a real config file. Every method is the binary's, minus the connecting: the
/// point of the tests that use it is the file, not the session. Two suites need one, and a
/// second copy would be a second thing to keep correct.
pub struct FileContexts {
    path: PathBuf,
    flags: Flags,
}

impl FileContexts {
    pub fn at(path: &std::path::Path) -> FileContexts {
        FileContexts {
            path: path.to_path_buf(),
            flags: Flags::default(),
        }
    }

    /// What the command line said about this run: the provenance no config file carries.
    pub fn with_flags(mut self, flags: Flags) -> FileContexts {
        self.flags = flags;
        self
    }

    fn load(&self) -> nutsh_config::Config {
        nutsh_config::load(&self.path).expect("a config this test wrote")
    }
    fn save(&self, cfg: &nutsh_config::Config) -> anyhow::Result<()> {
        Ok(nutsh_config::save(&self.path, cfg)?)
    }
}

impl Contexts for FileContexts {
    fn list(&self) -> anyhow::Result<Vec<ContextRow>> {
        Ok(Vec::new())
    }
    fn connect(
        &self,
        _req: ConnectRequest,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<Connected>> {
        Box::pin(async { anyhow::bail!("no contexts in this test") })
    }
    fn remove(&self, _name: &str) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("no contexts in this test")
    }
    fn set_skin(&self, name: &str) -> anyhow::Result<()> {
        let mut cfg = self.load();
        cfg.skin.name = Some(name.to_string());
        self.save(&cfg)
    }
    fn set_mouse(&self, enabled: bool) -> anyhow::Result<()> {
        let mut cfg = self.load();
        cfg.mouse = enabled;
        self.save(&cfg)
    }
    fn set_header(&self, header: nutsh_config::Header) -> anyhow::Result<()> {
        let mut cfg = self.load();
        cfg.header = header;
        self.save(&cfg)
    }
    fn nav_hidden(&self) -> Vec<String> {
        self.load().nav.hide
    }
    fn nav_hide_unserved(&self) -> bool {
        self.load().nav.hide_unserved
    }
    fn set_nav_hidden(&self, hide: &[String]) -> anyhow::Result<()> {
        let mut cfg = self.load();
        cfg.nav.hide = hide.to_vec();
        self.save(&cfg)
    }
    fn settings(&self) -> Vec<Setting> {
        nutsh_core::contexts::settings_of(&self.load(), &self.flags)
    }
    fn set_flag(&self, id: SettingId, on: bool) -> anyhow::Result<()> {
        let mut cfg = self.load();
        match id {
            SettingId::Cache => cfg.cache = on,
            SettingId::HideUnserved => cfg.nav.hide_unserved = on,
            other => anyhow::bail!("{} is not written from the screen", other.label()),
        }
        self.save(&cfg)
    }
    fn refresh(&self) -> nutsh_config::Refresh {
        self.load().refresh
    }
    fn set_interval(
        &self,
        at: nutsh_core::contexts::Schedule<'_>,
        every: nutsh_config::Interval,
    ) -> anyhow::Result<()> {
        use nutsh_core::contexts::Schedule;
        let mut cfg = self.load();
        match at {
            Schedule::Kind(id) => {
                cfg.refresh.kinds.insert(id.to_string(), every);
            }
            Schedule::Namespace(ns) => {
                cfg.refresh.namespaces.insert(ns.to_string(), every);
            }
            Schedule::Everything => cfg.refresh.default = Some(every),
        }
        self.save(&cfg)
    }
}
