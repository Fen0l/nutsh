//! The meter in the status line: where it sits, when it goes, and what it is allowed to say.

mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::App;
use nutsh_tui::app::Config;
use ratatui::buffer::Buffer;

/// The VM list and the cluster list as the client sends them, for the mock's path-exact
/// scripting. The table polls the first; the session resolves its clusters through the second.
const VMS: &str = "/vmm/v4.3/ahv/config/vms";
const CLUSTERS: &str = "/clustermgmt/v4.3/config/clusters";

/// The sync indicator's cell, which is what the right-hand block is anchored on. Sixteen
/// columns: `◌ cached 12m ` is thirteen of them.
const SYNC_WIDTH: usize = 16;

/// A flash sized to discriminate. It has to be longer than the 27 columns the flash would
/// have at width 70 if the meter's cell were reserved unconditionally - otherwise the test
/// below passes on the very bug it guards - and no longer than the 47 columns it really has at
/// width 90, where the meter is drawn and the flash's cell is 48 wide.
const FLASH: &str = "a status long enough to notice a lost column";

/// The per-file helper every `crates/tui/tests/*.rs` already has, in the shape `sidebar.rs`
/// spells it. `common` owns `session`, `config` and `NoContexts`; the constructor is local.
async fn app(pc: &MockPc) -> App {
    app_with(pc, common::config()).await
}

async fn app_with(pc: &MockPc, config: Config) -> App {
    App::open(
        common::session(pc).await,
        Box::new(common::NoContexts),
        "vm",
        config,
    )
    .unwrap()
}

/// The status line as its columns rather than as trimmed text: what the `[meter]` filter keeps
/// out of the snapshots is exactly the padding, so a test that asserts *where* the right-hand
/// block sits has to read the row whole.
fn status_line(buf: &Buffer) -> String {
    let width = buf.area.width;
    (0..buf.area.height)
        .map(|y| common::row_text(buf, 0..width, y))
        // Every state the indicator can emit, not the two the first test needed: a frame that
        // has settled and then paused says `⏸ idle` and nothing else, and looking for it by
        // `● live` would report "no status line" on a status line that is right there.
        .find(|row| {
            row.contains("● live")
                || row.contains("○ syncing")
                || row.contains("◌ cached")
                || row.contains("⏸ idle")
        })
        .unwrap_or_else(|| panic!("no status line on the frame:\n{}", common::text_of(buf)))
}

/// The last `SYNC_WIDTH` columns of a line. Every glyph the status line draws is one cell
/// wide, so a character count is a column count here.
fn tail(line: &str) -> String {
    let start = line.chars().count() - SYNC_WIDTH;
    line.chars().skip(start).collect()
}

/// A wide frame carries both halves, right of the flash and left of the sync indicator.
#[tokio::test]
async fn a_wide_frame_shows_the_rate_and_the_ratio() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let frame = app.snapshot(120, 20).unwrap();
    let line = frame
        .lines()
        .find(|l| l.contains("req/s"))
        .unwrap_or_else(|| panic!("no meter on the frame:\n{frame}"));
    assert!(line.contains("cached"), "{line}");
    assert!(
        line.trim_end().ends_with("● live") || line.trim_end().ends_with("○ syncing"),
        "the sync indicator keeps the right-hand end: {line}"
    );
}

/// Where the block sits, which no snapshot pins: the `[meter]` filter eats the right-aligned
/// padding on purpose, so a drift that moved both readouts twenty columns left would leave
/// every `.snap` byte-identical. The indicator owns the last twelve columns of a full-width
/// line and the meter ends before them.
#[tokio::test]
async fn the_right_hand_block_keeps_its_columns() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let buf = app.frame(120, 20).unwrap();
    let line = status_line(&buf);
    assert_eq!(line.chars().count(), 120, "the line is the frame: {line:?}");
    assert_eq!(tail(&line), "         ● live ", "{line:?}");
    let left: String = line.chars().take(120 - SYNC_WIDTH).collect();
    assert!(
        left.trim_end().ends_with("cached"),
        "the meter ends before the indicator's cell: {left:?}"
    );
}

/// At 90 columns the ratio goes and the rate stays; at 70 the whole field goes, and neither
/// the flash nor the sync indicator is clipped for it - the flash keeps a status of its own
/// whole, and the indicator keeps every column of its label.
///
/// The three widths are the three things a reserved cell would break, and `FLASH` is sized so
/// that each of them fails if it is: at 90 the cell is real and the flash still fits, at 70
/// the flash would lose 26 of its 57 columns to a cell with nothing in it, and at 20 there is
/// no room for a third cell at all and the indicator itself is what gets cut.
#[tokio::test]
async fn the_meter_degrades_by_width_without_clipping_its_neighbours() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.status = Some(FLASH.into());

    let ninety = status_line(&app.frame(90, 20).unwrap());
    assert!(ninety.contains("req/s"), "{ninety:?}");
    assert!(!ninety.contains("cached"), "{ninety:?}");
    assert!(ninety.contains(FLASH), "the flash is whole: {ninety:?}");
    assert_eq!(tail(&ninety), "         ● live ", "{ninety:?}");

    let seventy = status_line(&app.frame(70, 20).unwrap());
    assert!(!seventy.contains("req/s"), "{seventy:?}");
    assert!(seventy.contains(FLASH), "the flash is whole: {seventy:?}");
    assert_eq!(tail(&seventy), "         ● live ", "{seventy:?}");

    let twenty = status_line(&app.frame(20, 20).unwrap());
    assert!(
        twenty.contains("● live "),
        "the indicator keeps its label on a frame with room for two cells: {twenty:?}"
    );
}

/// Without a session nothing has been asked of anything, so the meter says nothing - the same
/// rule `sync_indicator` already follows.
#[tokio::test]
async fn a_disconnected_app_shows_no_meter() {
    let app = App::disconnected(
        Box::new(common::NoContexts),
        "vm",
        Some("nothing to connect to".into()),
        None,
        common::config(),
    );
    let frame = app.snapshot(120, 20).unwrap();
    assert!(!frame.contains("req/s"), "{frame}");
}

/// `--snapshot` renders one frame and exits, and a request rate is a timing value, not a
/// golden frame. The flag reaches the app through `Config`, so the suppression is a property
/// of the run rather than of the renderer, and the TUI's own frame tests keep their meter.
#[tokio::test]
async fn a_snapshot_run_renders_no_meter() {
    let pc = MockPc::builder().start().await;
    let mut app = app_with(
        &pc,
        Config {
            snapshot: true,
            ..common::config()
        },
    )
    .await;
    common::settle(&mut app).await;
    let frame = app.snapshot(120, 20).unwrap();
    assert!(!frame.contains("req/s"), "{frame}");
    assert!(
        frame.contains("live") || frame.contains("syncing"),
        "{frame}"
    );
}

/// The colour is the headline of the whole field and a text snapshot cannot see one, so the
/// three states are read off the buffer. A healthy session is the muted default.
#[tokio::test]
async fn a_healthy_session_leaves_the_meter_muted() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 20).unwrap();
    assert_eq!(common::style_of(&buf, "req/s").fg, Some(p.subtext0));
}

/// Amber says the budget is pacing us. A served 429 drains the bucket and lands in the same
/// ten-second window the meter reads, so the state is the test's to make, not the clock's.
#[tokio::test]
async fn a_rate_limit_turns_the_meter_amber() {
    let pc = MockPc::builder().rate_limit_once(VMS).start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 20).unwrap();
    assert_eq!(common::style_of(&buf, "req/s").fg, Some(p.peach));
}

/// And red says the pipe broke - a 5xx is the far end not answering, which no row can report.
/// Red beats amber: this session is both paced (the 429 on its cluster resolution) and broken
/// (the 500 on its table), and the broken half is the one worth the colour.
#[tokio::test]
async fn a_failure_turns_the_meter_red_over_the_amber() {
    let pc = MockPc::builder()
        .rate_limit_once(CLUSTERS)
        .fail_path(VMS, 500)
        .start()
        .await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 20).unwrap();
    assert_eq!(common::style_of(&buf, "req/s").fg, Some(p.red));
}

/// The exit criterion, in one test: a second run paints rows and names in the first frame,
/// badged with their age, and issues **zero** negotiation requests.
#[tokio::test]
async fn a_second_run_paints_before_the_network_answers_and_negotiates_nothing() {
    let state = tempfile::tempdir().unwrap();
    let pc = MockPc::builder().start().await;

    // First run: connect, settle, and write.
    let mut first = common::app_with_cache(&pc, state.path()).await;
    common::settle(&mut first).await;
    first.write_cache_now().expect("a write").await.unwrap();
    drop(first);
    let dirs: Vec<_> = std::fs::read_dir(state.path().join("nutsh/cache"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(dirs.len(), 1, "a directory appeared");

    // Second run against the same host:port - which is half the cache identity - with the VM
    // list two seconds behind: the frame at t=0 already has rows.
    let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
    pc.set_delay(vms.list_path, std::time::Duration::from_secs(2));
    let before = std::time::Instant::now();
    let second = common::app_with_cache(&pc, state.path()).await;
    let frame = second.snapshot(120, 20).unwrap();
    assert!(
        before.elapsed() < std::time::Duration::from_secs(1),
        "no list was waited on"
    );
    assert!(frame.contains("web-01"), "rows from the cache:\n{frame}");
    assert!(
        frame.contains("◌ cached"),
        "and the badge that dates them:\n{frame}"
    );
    // The domain manager read, plus at most the first VM list already in flight behind the
    // frame. A negotiation would be twenty probes and this number would be past that.
    assert!(
        second
            .live
            .as_ref()
            .unwrap()
            .session
            .client
            .metrics()
            .started()
            <= 2,
        "not one negotiation probe"
    );
}

/// Five minutes with nobody there: nothing is polling, so the indicator says so instead of
/// `● live`, and the first key ends both the pause and the word.
///
/// Settled first, and asserted settled: a table on its first fetch says `○ syncing` anyway, so
/// on an unsettled app `!contains("● live")` would pass with the branch deleted and the
/// precedence - idle outranks a table that really has completed its cycle - would go untested.
#[tokio::test]
async fn the_indicator_says_idle_while_the_pause_holds() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let frame = app.snapshot(120, 20).unwrap();
    assert!(frame.contains("● live"), "the table has settled:\n{frame}");

    app.live
        .as_ref()
        .expect("a session")
        .scheduler
        .set_idle(true);
    let frame = app.snapshot(120, 20).unwrap();
    assert!(frame.contains("⏸ idle"), "{frame}");
    assert!(!frame.contains("● live"), "{frame}");

    // The waking key is delivered as an ordinary key; there is no "press any key to resume".
    app.handle(nutsh_tui::Key::Char('j'));
    assert!(!app.live.as_ref().unwrap().scheduler.is_idle());
    let frame = app.snapshot(120, 20).unwrap();
    assert!(!frame.contains("⏸ idle"), "{frame}");
    assert!(frame.contains("● live"), "{frame}");
}

/// Entering the pause, which is what `App::tick` decides: five minutes on the monotonic clock
/// it is handed, with no key, resize, action or context switch since the app was built. The
/// clock is a parameter for exactly this - a test may not sit still for five minutes.
#[tokio::test]
async fn five_silent_minutes_start_the_pause_and_the_next_key_ends_it() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let scheduler_idle = |app: &App| app.live.as_ref().expect("a session").scheduler.is_idle();
    assert!(!scheduler_idle(&app), "somebody has just opened it");

    let five_minutes_on = std::time::Instant::now() + nutsh_core::scheduler::IDLE_AFTER;
    app.tick(common::now(), five_minutes_on);
    assert!(scheduler_idle(&app), "nobody has touched it since");
    let frame = app.snapshot(120, 20).unwrap();
    assert!(frame.contains("⏸ idle"), "{frame}");

    // A key ends it, and the tick that follows does not put it back: the five minutes are
    // measured from the last input, which the key has just restamped.
    app.handle(nutsh_tui::Key::Char('j'));
    assert!(!scheduler_idle(&app));
    app.tick(common::now(), std::time::Instant::now());
    assert!(!scheduler_idle(&app));
    let frame = app.snapshot(120, 20).unwrap();
    assert!(!frame.contains("⏸ idle"), "{frame}");
}
