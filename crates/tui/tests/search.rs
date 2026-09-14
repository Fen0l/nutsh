mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key};

async fn app(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap()
}

fn kind(id: &str) -> &'static nutsh_catalog::Kind {
    nutsh_catalog::kind(id).expect("the catalog has it")
}

/// Two kinds in the store: the VM table the app opens on, and the Hosts table opened over it.
async fn vms_and_hosts(pc: &MockPc) -> App {
    let mut app = app(pc).await;
    common::settle(&mut app).await;
    app.open_root(kind("clustermgmt.config.Host"));
    common::settle(&mut app).await;
    app.open_root(kind("vmm.ahv.config.Vm"));
    common::settle(&mut app).await;
    // The frame includes the header summary, which the stats poller fills on its own cycle.
    common::settle_stats(&mut app).await;
    app
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle(Key::Char(c));
    }
}

/// `:search` looks across every kind already loaded and says so in the same breath. A search
/// that covered two kinds of two hundred and sixty-two while looking complete would be the
/// exact lie this release has spent its commits removing.
#[tokio::test]
async fn search_groups_its_results_and_states_its_reach() {
    let pc = MockPc::builder().start().await;
    let mut app = vms_and_hosts(&pc).await;
    let before = pc.requests().len();

    app.handle(Key::Char(':'));
    type_str(&mut app, "search 203.0.113");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Search);
    let frame = app.snapshot(120, 24).unwrap();
    common::settings(&pc).bind(|| insta::assert_snapshot!("search_results", frame.clone()));

    assert!(frame.contains("results for \"203.0.113\""), "{frame}");
    // Grouped by kind, each row named and shown what it matched on.
    assert!(frame.contains("Virtual Machines"), "{frame}");
    assert!(
        frame.contains("web-01") && frame.contains("IP 203.0.113.11"),
        "{frame}"
    );
    assert!(frame.contains("Hosts"), "{frame}");
    // The curator's name for the field that answered, because a Host has four addresses and a
    // row that showed one without saying which leaves the reader guessing.
    assert!(
        frame.contains("ahv-node-1") && frame.contains("CVM IP 203.0.113.201"),
        "{frame}"
    );
    // The reach, plainly.
    assert!(
        frame.contains("searched 2 loaded kinds · 230 not loaded"),
        "{frame}"
    );
    // And nothing was fetched to answer it.
    assert_eq!(pc.requests().len(), before, "a search asks for nothing");
}

/// `⏎` on a result opens the entity: its kind's table, with the cursor on the row.
#[tokio::test]
async fn enter_on_a_result_opens_that_entity() {
    let pc = MockPc::builder().start().await;
    let mut app = vms_and_hosts(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "search ahv-node-1");
    app.handle(Key::Enter);
    // The first row is the group header; the cursor starts on the first hit under it.
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(app.view().unwrap().key.kind.id, "clustermgmt.config.Host");
    assert_eq!(app.selected_name(), Some("ahv-node-1"));
}

/// A term nothing matches still says what was looked at: an empty list that does not say how
/// far it reached is indistinguishable from one that searched everything and found nothing.
#[tokio::test]
async fn a_search_that_finds_nothing_still_states_its_reach() {
    let pc = MockPc::builder().start().await;
    let mut app = vms_and_hosts(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "search nothing-is-called-this");
    app.handle(Key::Enter);
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains("nothing matches"), "{frame}");
    assert!(
        frame.contains("searched 2 loaded kinds · 230 not loaded"),
        "{frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
}

/// `:search` with nothing after it has nothing to look for, and says so rather than opening an
/// empty list.
#[tokio::test]
async fn search_with_no_term_asks_for_one() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "search");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    let frame = app.snapshot(120, 15).unwrap();
    assert!(frame.contains("search: type what to look for"), "{frame}");
}

/// Whatever is bound is advertised. `:search` is one of the palette's commands and completes
/// like the rest of them, and the `?` overlay names both surfaces on one row.
#[tokio::test]
async fn both_surfaces_are_advertised() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "sear");
    let palette = app.snapshot(120, 20).unwrap();
    assert!(
        palette.contains("search"),
        "the palette ranks it: {palette}"
    );
    // `⇥` completes it, exactly as it completes every other command.
    app.handle(Key::Tab);
    app.handle(Key::Esc);

    app.handle(Key::Char('?'));
    let overlay = app.snapshot(120, 27).unwrap();
    assert!(overlay.contains("/  :search"), "{overlay}");
    assert!(overlay.contains("every loaded kind"), "{overlay}");
    assert!(
        overlay.contains("/  :search   match name, IP or id here, or across every loaded kind"),
        "the whole row, because a clipped line leaves no trace on a frame: {overlay}"
    );
}

/// The results are a snapshot of the store as it was. A poll landing underneath must not
/// reshuffle a list somebody is reading, and `⏎` opens the row by its identifier either way.
#[tokio::test]
async fn a_poll_underneath_does_not_move_the_results() {
    let pc = MockPc::builder().start().await;
    let mut app = vms_and_hosts(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "search web");
    app.handle(Key::Enter);
    let before = results_box(&app);
    app.inject_rows(40);
    assert_eq!(
        results_box(&app),
        before,
        "forty new rows under the box change nothing on it"
    );
    // The table under it did move, which is what makes the assertion above worth making.
    assert!(app.snapshot(120, 24).unwrap().contains("Count:      40"));
}

/// The results box alone, without the menu beside it or the header above it: what a poll
/// landing underneath is allowed to change, and what it is not.
fn results_box(app: &App) -> String {
    app.snapshot(120, 24)
        .unwrap()
        .lines()
        .filter(|l| l.contains("results for") || l.contains('│'))
        .map(|l| l.chars().skip(24).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
