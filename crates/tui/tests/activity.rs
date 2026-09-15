//! `:activity`: the requests this session made, and the tables it holds.

mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key};

async fn app(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app
}

fn open(app: &mut App) {
    app.handle(Key::Char(':'));
    for c in "activity".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

/// The requests tab is the funnel, row by row: the VM list this app opened on, with its status,
/// its round trip and the word for what it carried. The tables tab is the store: the VM table,
/// its rows, and that it came off the wire moments ago.
#[tokio::test]
async fn the_screen_lists_the_requests_and_the_tables() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let made = pc.requests().len();

    open(&mut app);
    assert_eq!(app.mode, Mode::Activity);
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains(" activity · requests"), "{frame}");
    assert!(
        frame.contains(&format!("[{made}]")),
        "one row per request made: {frame}"
    );
    assert!(frame.contains("GET"), "{frame}");
    assert!(frame.contains("200"), "{frame}");
    assert!(frame.contains("/api/vmm/"), "{frame}");
    assert!(
        frame.contains("presented") || frame.contains("session"),
        "which credential went out: {frame}"
    );
    assert!(
        frame
            .lines()
            .filter(|l| l.contains("/api/"))
            .all(|l| !l.contains("127.0.0.1")),
        "a path, never a URL: {frame}"
    );

    app.handle(Key::Tab);
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains(" activity · tables"), "{frame}");
    let vm = frame
        .lines()
        .find(|l| l.contains("Virtual Machines"))
        .unwrap_or_else(|| panic!("the VM table is held: {frame}"));
    assert!(vm.contains(" 3 "), "three rows: {vm}");
    assert!(vm.contains("wire"), "{vm}");
    assert!(vm.contains("ok"), "{vm}");
    assert!(!vm.contains("never"), "{vm}");

    app.handle(Key::Tab);
    assert!(
        app.snapshot(120, 24)
            .unwrap()
            .contains(" activity · requests")
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
}

/// A list path this Prism Central does not have is a 404 in the ring and `not served` on the
/// table, in words rather than colours, so the screen answers "why is this table empty".
#[tokio::test]
async fn a_missing_list_path_is_a_404_and_a_not_served_table() {
    let pc = MockPc::builder()
        .missing_path("/clustermgmt/v4.3/config/hosts")
        .start()
        .await;
    let mut app = app(&pc).await;
    app.open_root(nutsh_catalog::kind("clustermgmt.config.Host").expect("hosts"));
    common::settle(&mut app).await;

    open(&mut app);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("404") && l.contains("/config/hosts")),
        "{frame}"
    );
    app.handle(Key::Tab);
    let frame = app.snapshot(120, 30).unwrap();
    let hosts = frame
        .lines()
        // The header's `Kind:` line names Hosts too; the row is the one inside the box.
        .find(|l| l.contains("Hosts") && !l.contains("Kind:"))
        .unwrap_or_else(|| panic!("{frame}"));
    assert!(hosts.contains("not served"), "{hosts}");
    assert!(hosts.contains("never"), "no cycle ever completed: {hosts}");
}
