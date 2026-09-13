//! The header's two heights: the full box with its hint grid and stats block, and the one line
//! a short terminal gets instead.

mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::App;
use nutsh_tui::app::{Config, Header};

/// `common::config()` pins the full header for every other suite; this one is about the choice,
/// so it asks for the default the config file's own default gives.
async fn app(pc: &MockPc) -> App {
    app_with(pc, config_with(Header::Auto)).await
}

async fn app_with(pc: &MockPc, config: Config) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(session, Box::new(common::NoContexts), "vm", config).unwrap();
    common::settle(&mut app).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app
}

fn config_with(header: Header) -> Config {
    Config {
        header,
        ..common::config()
    }
}

/// How many rows of the table a frame shows. Counted off the names `App::inject_rows` writes,
/// so the answer is the number of rows drawn rather than the number the store holds.
fn table_rows(frame: &str) -> usize {
    frame.lines().filter(|l| l.contains("vm-0")).count()
}

/// The seven-line box costs nine rows of chrome with the prompt and the status line, and an
/// eighty by twenty-four terminal has fifteen left before the table's own borders. One line
/// gives six of them back.
#[tokio::test]
async fn a_short_frame_folds_the_header_and_a_tall_one_keeps_it() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.inject_rows(40);

    let short = app.snapshot(120, 24).unwrap();
    assert!(
        !short.contains("Context:"),
        "the folded line has no field labels: {short}"
    );
    assert!(!short.contains("⏎ drill"), "nor the hint grid: {short}");
    let tall = app.snapshot(120, 40).unwrap();
    assert!(tall.contains("Context:"), "{tall}");
    assert!(tall.contains("⏎ drill"), "{tall}");

    // The point of the exercise, measured on one height so nothing but the header differs:
    // six of the seven header rows come back as rows of the table.
    let folded = app.snapshot(120, 24).unwrap();
    app.set_header(Header::Full);
    let boxed = app.snapshot(120, 24).unwrap();
    assert_eq!(table_rows(&folded), table_rows(&boxed) + 6, "{folded}");
}

/// The threshold is derived from the tallest thing the app draws inside the body - the `?`
/// overlay - plus the table chrome under it, so the frame that keeps its full header is the
/// shortest one where the overlay still has a table to be an overlay over.
#[tokio::test]
async fn the_threshold_is_where_the_help_overlay_stops_fitting() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let at = nutsh_tui::ui::FULL_HEADER_ROWS;

    let full = app.snapshot(120, at).unwrap();
    assert!(full.contains("Context:"), "{at} rows keeps it: {full}");
    let folded = app.snapshot(120, at - 1).unwrap();
    assert!(
        !folded.contains("Context:"),
        "one row shorter folds it: {folded}"
    );

    // And the overlay it was derived from is drawn whole at the threshold: its last line is
    // the one that would be lost, and a clipped line leaves no trace on a frame.
    app.handle(nutsh_tui::Key::Char('?'));
    let help = app.snapshot(120, at).unwrap();
    assert!(help.contains("^c quit"), "{help}");
}

/// Context, cluster scope, read-only and how fresh the rows are: the four facts the folded line
/// exists to keep, plus what the body is showing so the reader knows where they are.
#[tokio::test]
async fn the_folded_line_names_the_context_the_cluster_and_the_freshness() {
    let pc = MockPc::builder().start().await;
    let mut session = common::session(&pc).await;
    session.readonly = true;
    session.context = Some("lab".to_string());
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        config_with(Header::Auto),
    )
    .unwrap();
    common::settle(&mut app).await;
    let frame = app.snapshot(120, 24).unwrap();
    let line = frame.lines().next().expect("a frame has a first line");
    assert!(line.contains("lab"), "the context: {line}");
    assert!(line.contains("<all>"), "the cluster scope: {line}");
    assert!(line.contains("read-only"), "the session's mode: {line}");
    assert!(line.contains("VMs"), "what is open: {line}");
    assert!(
        line.contains("live") || line.contains("syncing") || line.contains("cached"),
        "how fresh the rows are: {line}"
    );
}

/// The grid goes with the box, and it is where `a actions` was advertised. The prompt line and
/// the `?` overlay still name it, so nothing that made the app discoverable is lost with the
/// six rows.
#[tokio::test]
async fn folding_the_grid_away_leaves_the_action_key_advertised() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let frame = app.snapshot(120, 24).unwrap();
    assert!(!frame.contains("a actions"), "the grid is gone: {frame}");
    let prompt = frame
        .lines()
        .nth_back(1)
        .expect("the prompt line is above the status line");
    assert!(prompt.contains("a:actions"), "{prompt}");
    app.handle(nutsh_tui::Key::Char('?'));
    let help = app.snapshot(120, 24).unwrap();
    assert!(help.contains("actions for this row"), "{help}");
}

/// The size decides unless the file says otherwise, and the file can say it either way.
#[tokio::test]
async fn the_config_key_overrides_the_size_in_both_directions() {
    let pc = MockPc::builder().start().await;
    let compact = app_with(&pc, config_with(Header::Compact)).await;
    assert!(
        !compact.snapshot(120, 40).unwrap().contains("Context:"),
        "a tall frame still folds when the file asked for it"
    );
    let full = app_with(&pc, config_with(Header::Full)).await;
    assert!(
        full.snapshot(120, 24).unwrap().contains("Context:"),
        "and a short one keeps the box"
    );
}

/// The two heights, side by side, on the terminal the product audit measured. Everything the
/// box said that nothing else on the frame says is still there; six rows of it are table.
#[tokio::test]
async fn the_two_heights_at_eighty_by_twenty_four() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("folded_80x24", app.snapshot(80, 24).unwrap()));
    settings.bind(|| insta::assert_snapshot!("folded_120x24", app.snapshot(120, 24).unwrap()));
    app.set_header(Header::Full);
    settings.bind(|| insta::assert_snapshot!("boxed_80x24", app.snapshot(80, 24).unwrap()));
}

/// `:header` cycles the three settings and says which it landed on, the way `:mouse` reports
/// the state it toggled to.
#[tokio::test]
async fn the_header_command_cycles_the_three_settings() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    for expect in ["compact", "full", "auto"] {
        app.handle(nutsh_tui::Key::Char(':'));
        for c in "header".chars() {
            app.handle(nutsh_tui::Key::Char(c));
        }
        app.handle(nutsh_tui::Key::Enter);
        assert_eq!(app.status.as_deref(), Some(&format!("header {expect}")[..]));
    }
}

/// The line degrades by giving the breadcrumb up, not by dropping the facts off its end: a
/// narrow frame keeps `[read-only]`, which is the one thing on it that governs what the session
/// can do, and cuts `Compute & Storage › VMs` from the left instead.
#[tokio::test]
async fn a_narrow_frame_cuts_the_breadcrumb_and_keeps_the_facts() {
    let pc = MockPc::builder().start().await;
    let mut session = common::session(&pc).await;
    session.readonly = true;
    session.context = Some("lab".to_string());
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        config_with(Header::Auto),
    )
    .unwrap();
    common::settle(&mut app).await;
    for width in 30..=120u16 {
        let frame = app.snapshot(width, 24).unwrap();
        let line = frame.lines().next().expect("a first line");
        assert!(
            line.chars().count() <= usize::from(width),
            "{width}: the line wrapped: {line}"
        );
        // The order they are given up in, cheapest first, whatever the width: the count is on
        // the table's own title bar too, and `[read-only]` is on nothing else at all.
        if line.contains("[3]") {
            assert!(line.contains("cluster:"), "{width}: {line}");
        }
        if line.contains("cluster:") {
            assert!(line.contains("ctx:lab"), "{width}: {line}");
        }
        if line.contains("ctx:lab") {
            assert!(line.contains("[read-only]"), "{width}: {line}");
        }
    }
    // At the terminal the audit measured, all of it fits.
    let line = app.snapshot(80, 24).unwrap();
    let line = line.lines().next().expect("a first line");
    for fact in ["[3]", "cluster:<all>", "ctx:lab", "[read-only]"] {
        assert!(line.contains(fact), "{fact}: {line}");
    }
    // And nothing panics all the way down, where even the facts stop fitting.
    for width in 1..40 {
        app.snapshot(width, 24).unwrap();
        app.snapshot(width, 3).unwrap();
    }
}
