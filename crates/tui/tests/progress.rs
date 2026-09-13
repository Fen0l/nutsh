//! What a table says while it is walking: the title's numbers and the body's sentence.

mod common;

use std::time::Duration;

use nutsh_mockpc::{Gate, MockPc};
use nutsh_tui::{App, Key};

/// The busiest collection a Prism Central keeps: 181 825 audits is 1819 pages, which is what a row budget
/// and a walking note exist for.
const AUDITS: &str = "/monitoring/v4.3/serviceability/audits";

/// One frame, at the size these tests are about: fourteen rows of terminal, nine of them chrome,
/// is five rows of table. The note is drawn where the first row would go, so a frame that leaves
/// the table only its border and its header row has nowhere to put it - twelve is exactly that
/// frame, and `a_frame_with_no_room_for_a_row_draws_no_note` holds it to that.
fn frame(app: &App) -> String {
    app.snapshot(120, 14).unwrap()
}

/// The audits table, emptied. Whatever either fixture tree holds for the kind, these tests are
/// about what a body with **no** rows says, so the table is emptied by hand rather than by
/// depending on a fixture staying absent.
async fn audits(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "audits",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app.inject_entities(Vec::new());
    app
}

/// The 0-based line a needle is on, so a test can assert what is *above* what.
fn line_of(frame: &str, needle: &str) -> usize {
    frame
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle} is not on the frame:\n{frame}"))
}

/// Three places, three sentences. The title carries the numbers, the body says what is happening
/// right now, and the header's marker - which this does not touch - goes on saying what the
/// table *is*.
#[tokio::test]
async fn a_walking_table_says_what_it_is_doing_in_its_title_and_its_body() {
    let pc = MockPc::builder().start().await;
    let mut app = audits(&pc).await;

    // Nothing staged. The budget comes from the catalog with no server round trip, so this is on
    // the very first frame - which is the whole point for a collection of 181 825 rows.
    app.inject_walking(0, 181_825, Duration::ZERO);
    let f = frame(&app);
    assert!(f.contains("listing up to 500 rows…"), "{f}");
    assert!(f.contains("[↻ 0 rows · 0s]"), "{f}");
    // Above, not merely present: the note is drawn over the first *row*, and a note that landed
    // on the heading row would take the columns off the frame that announces them.
    assert!(
        line_of(&f, "OPERATION") < line_of(&f, "listing up to"),
        "the column headings stay above the note: {f}"
    );

    // A page staged: the server's own count is on the frame long before the walk that reports it
    // ends, which is what `count()` reading `staged_total` bought.
    app.inject_walking(100, 181_825, Duration::from_secs(1));
    let f = frame(&app);
    assert!(f.contains("listing… 100 of 181825 rows"), "{f}");
    assert!(f.contains("[0/181825]"), "{f}");
    assert!(f.contains("[↻ 100 rows · 1s]"), "{f}");

    // The elapsed time is `cell::span`, the one spelling of a length of time in this program.
    app.inject_walking(400, 181_825, Duration::from_secs(3));
    assert!(frame(&app).contains("· 3s]"), "3s");
    app.inject_walking(400, 181_825, Duration::from_secs(125));
    assert!(frame(&app).contains("· 2m]"), "2m");
}

/// The note takes a row's place, so a table with no room for a row has no room for it. The
/// title's bracket is not a row and stays.
#[tokio::test]
async fn a_frame_with_no_room_for_a_row_draws_no_note() {
    let pc = MockPc::builder().start().await;
    let mut app = audits(&pc).await;
    app.inject_walking(0, 181_825, Duration::ZERO);
    let squashed = app.snapshot(120, 12).unwrap();
    assert!(
        !squashed.contains("listing"),
        "no row fits, so the note does not either: {squashed}"
    );
    assert!(squashed.contains("[↻ 0 rows · 0s]"), "{squashed}");
    assert!(frame(&app).contains("listing"), "one row taller, it fits");
}

/// Dim, beside the count, in both places - asserted through the buffer, because a text snapshot
/// cannot tell a dim sentence from a bright one.
#[tokio::test]
async fn the_bracket_and_the_note_are_dim() {
    let pc = MockPc::builder().start().await;
    let mut app = audits(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    app.inject_walking(0, 181_825, Duration::ZERO);
    let buf = app.frame(120, 14).unwrap();
    assert_eq!(
        common::style_of(&buf, "↻").fg,
        Some(p.overlay1),
        "the title's second bracket"
    );
    assert_eq!(
        common::style_of(&buf, "[0/181825]").fg,
        Some(p.yellow),
        "the count beside it keeps its own colour"
    );
    assert_eq!(
        common::style_of(&buf, "listing up to").fg,
        Some(p.overlay1),
        "the body's note"
    );
}

/// The two states that are not about a cycle in flight, and what happens when one starts anyway.
#[tokio::test]
async fn a_settled_table_with_no_rows_says_so_and_a_failed_one_says_why() {
    let pc = MockPc::builder().start().await;
    let mut app = audits(&pc).await;
    let f = frame(&app);
    assert!(
        f.contains("no rows"),
        "the cycle completed and found none: {f}"
    );
    assert!(!f.contains("listing"), "{f}");
    assert!(!f.contains("[↻"), "no cycle is in flight: {f}");

    app.inject_error("audits are unavailable");
    let f = frame(&app);
    // In the body, not only on the status line, which has carried the last cycle's error all
    // along: a table with no rows at all has room to say why it has none where the rows would be.
    // A body line starts inside the menu's border; the status line starts with a space.
    assert!(
        f.lines()
            .any(|l| l.starts_with('│') && l.contains("audits are unavailable")),
        "{f}"
    );

    // The retry takes the body back. `Update::Started` does not clear the error - only a
    // `Complete` does - so without the in-flight state winning here the body would recite the
    // previous cycle's failure under a title whose clock is ticking. The error is not lost: it
    // is on the status line, in red, which is where it lived before there was a body note.
    app.inject_walking(0, 181_825, Duration::ZERO);
    let f = frame(&app);
    assert!(f.contains("listing up to 500 rows…"), "{f}");
    assert!(
        !f.lines()
            .any(|l| l.starts_with('│') && l.contains("audits are unavailable")),
        "the body says what is happening now: {f}"
    );
    assert!(
        f.lines()
            .next_back()
            .is_some_and(|l| l.contains("audits are unavailable")),
        "the status line still carries it: {f}"
    );
}

/// The Dashboard, from the palette, settled.
async fn dashboard(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    for c in "dashboard".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    common::settle_page(&mut app).await;
    app
}

/// A listing cycle in flight on one *pane*, which `App::inject_walking` cannot reach: it puts a
/// walk on the table view, and a page has none. The pane is emptied by a completed cycle first -
/// the fixture's own rows would otherwise leave the body nothing to say.
fn walk_pane(app: &mut App, pane: usize, staged: usize, total: u64) {
    use nutsh_core::store::Update;
    let key = app.page().expect("a page is open").panes[pane].key.clone();
    let live = app.live.as_mut().expect("a session");
    let generation = live.store.table(&key).generation + 1;
    live.store.apply(
        &key,
        Update::Page {
            generation,
            entities: Vec::new(),
            total: Some(0),
        },
    );
    live.store.apply(&key, Update::Complete { generation });
    let generation = generation + 1;
    live.store.apply(&key, Update::Started { generation });
    live.store.apply(
        &key,
        Update::Page {
            generation,
            entities: (0..staged)
                .map(|i| {
                    let name = format!("staged-{i:04}");
                    nutsh_prism::Entity::new(
                        key.kind,
                        serde_json::json!({ "extId": name, "name": name }),
                        None,
                    )
                })
                .collect(),
            total: Some(total),
        },
    );
}

/// A pane promises the number *it* asked for. `Subscription::sized` replaces the kind's budget
/// with the pane's own height, so the Dashboard's alerts pane - curated to 500 in the catalog -
/// fetches twenty, and `listing up to 500 rows…` would be a promise its walk never keeps.
#[tokio::test]
async fn a_walking_pane_names_the_budget_it_asked_for() {
    let alerts = nutsh_catalog::kind("monitoring.serviceability.Alert").expect("Alerts");
    assert_eq!(alerts.max_rows, Some(500), "the catalog's own budget");
    let pc = MockPc::builder().start().await;
    let mut app = dashboard(&pc).await;
    let i = app
        .page()
        .expect("the Dashboard is open")
        .panes
        .iter()
        .position(|p| p.key.kind.id == alerts.id)
        .expect("the alerts pane");
    let budget = app.page().unwrap().panes[i].budget().expect("a budget");
    assert!(budget < 500, "the pane narrowed it: {budget}");

    walk_pane(&mut app, i, 0, 181_825);
    let f = app.snapshot(200, 40).unwrap();
    assert!(f.contains(&format!("listing up to {budget} rows…")), "{f}");
    assert!(
        !f.contains("listing up to 500 rows…"),
        "not the kind's: {f}"
    );
    assert!(f.contains("[↻ 0 rows · 0s]"), "{f}");

    // And the pane's own count, from the staged page, in the pane's own title.
    walk_pane(&mut app, i, 7, 181_825);
    let f = app.snapshot(200, 40).unwrap();
    assert!(f.contains("listing… 7 of 181825 rows"), "{f}");
    assert!(f.contains("[↻ 7 rows · 0s]"), "{f}");
}

/// The exit criterion, end to end: a collection of five thousand rows says what it is doing in
/// the frame drawn before any response, and says the server's own count as soon as one page has
/// landed.
///
/// Against a held server rather than an injection, so the numbers come from the scheduler and
/// the catalog the way a real walk produces them.
#[tokio::test]
async fn the_first_frame_of_a_slow_walk_says_what_it_is_doing() {
    let gate = Gate::new();
    let pc = MockPc::builder()
        .repeat_fixture(AUDITS, 5000)
        .hold_path(AUDITS, &gate)
        .start()
        .await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "audits",
        common::config(),
    )
    .unwrap();
    // The cycle announces itself before its first request, which is the whole point: the
    // budget comes from the catalog and costs no round trip.
    tokio::time::timeout(Duration::from_secs(10), app.settle_walk(0))
        .await
        .expect("the cycle announces itself");
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("listing up to 500 rows…"), "{frame}");
    assert!(frame.contains("[↻ 0 rows · 0s]"), "{frame}");
    common::settings(&pc).bind(|| insta::assert_snapshot!("first_frame", frame));

    gate.release(1);
    tokio::time::timeout(Duration::from_secs(10), app.settle_walk(1))
        .await
        .expect("one page lands");
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("listing… 100 of 5000 rows"), "{frame}");
    assert!(frame.contains("[0/5000]"), "{frame}");
}

/// `ctrl-x` ends the page loop, lands the pages it already staged - they are paid for, and
/// throwing them away would make the keystroke a punishment - and pauses the subscription
/// until `ctrl-r`.
#[tokio::test]
async fn ctrl_x_lands_what_staged_stops_the_walk_and_ctrl_r_resumes_it() {
    let gate = Gate::new();
    let pc = MockPc::builder()
        .repeat_fixture(AUDITS, 5000)
        .hold_path(AUDITS, &gate)
        .start()
        .await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "audits",
        common::config(),
    )
    .unwrap();
    gate.release(1);
    tokio::time::timeout(Duration::from_secs(10), app.settle_walk(1))
        .await
        .expect("one page lands");

    // The page already on the wire still lands: the check is after the page, not before the
    // request it is answering.
    app.handle(Key::Ctrl('x'));
    gate.release(1);
    common::settle(&mut app).await;

    let staged = frame(&app);
    assert!(staged.contains("[200/5000]"), "the honest count: {staged}");
    assert!(!staged.contains("[↻"), "no cycle is in flight: {staged}");
    assert!(staged.contains("⏹ stopped"), "{staged}");

    // No further cycles. Two turns of the event loop with the gate wide open and the request
    // count stands where it was.
    let before = pc.requests_to(AUDITS).len();
    gate.release(10);
    app.drain().await;
    app.drain().await;
    assert_eq!(
        pc.requests_to(AUDITS).len(),
        before,
        "the subscription is paused"
    );

    app.handle(Key::Ctrl('r'));
    common::settle(&mut app).await;
    assert!(
        pc.requests_to(AUDITS).len() > before,
        "ctrl-r restarts the walk"
    );
    let resumed = frame(&app);
    assert!(
        !resumed.contains("⏹ stopped"),
        "and lifts the pause: {resumed}"
    );
}

/// A walk stopped before anything staged says so, rather than saying `no rows` - which would
/// be a claim about a collection nobody looked at.
#[tokio::test]
async fn stopping_before_the_first_page_says_so() {
    // Events have no fixture in either tree, so the mock answers an empty list for the
    // catalog's path and nothing stages.
    const EVENTS: &str = "/monitoring/v4.3/serviceability/events";
    let gate = Gate::new();
    let pc = MockPc::builder().hold_path(EVENTS, &gate).start().await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "events",
        common::config(),
    )
    .unwrap();
    app.handle(Key::Ctrl('x'));
    assert_eq!(app.status.as_deref(), Some("stopped"));
    gate.release(1);
    common::settle(&mut app).await;
    let screen = frame(&app);
    assert!(screen.contains("stopped before the first page"), "{screen}");
    assert!(screen.contains("⏹ stopped"), "{screen}");
}
