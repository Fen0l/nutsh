//! One frame per curated kind that has a fixture, against a real recorded shape - and two
//! against the demo estate, whose rows were written to make every conditional section of a
//! detail render somewhere.
//!
//! Nine of the eleven from the recordings. Volume groups and images have **no fixture in
//! either tree**; they are covered by unit tests over a hand-built entity in
//! `crates/core/tests/detail.rs` until a lab recording exists, which is a limitation stated
//! rather than papered over.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use nutsh_mockpc::{MockPc, Store};
use nutsh_tui::app::resolve_kind;
use nutsh_tui::{App, Key};

/// `crates/mockpc/fixtures-lab` where a recording exists, `crates/mockpc/fixtures` where only
/// the synthetic one does, so a snapshot shows a shape a Prism Central really returned.
fn tree(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../mockpc/{name}"))
}

/// Open `kind`, put the cursor on the first row, and open its detail.
async fn detail(pc: &MockPc, kind: &str) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        kind,
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    // The frame includes the header summary; without this its counters are a race.
    common::settle_stats(&mut app).await;
    app.handle(Key::Char('y'));
    // A pane over a kind with a single-entity GET waits for that document, because that is
    // what it is composed from. The flat Host has no get endpoint, so nothing is in flight and
    // there is nothing to wait for: its pane is composed from the list row, which is whole.
    if resolve_kind(kind).expect("a kind").get_path.is_some() {
        common::settle(&mut app).await;
    }
    app
}

/// Open `kind`, narrow it with `/term` to the one row the story is about, and open that row's
/// detail. The filter is the table's own and asks the mock for nothing.
async fn detail_of(pc: &MockPc, kind: &str, term: &str) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        kind,
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    common::settle_stats(&mut app).await;
    app.handle(Key::Char('/'));
    for c in term.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    app.handle(Key::Char('y'));
    if resolve_kind(kind).expect("a kind").get_path.is_some() {
        common::settle(&mut app).await;
    }
    // A failed task's pane asks once more, for the alerts around its start; the frame is not
    // settled until that window has answered, or the section reads `asking…`.
    if kind == "task" {
        tokio::time::timeout(Duration::from_secs(5), app.settle_once_list())
            .await
            .expect("the alerts window answers");
    }
    app
}

/// The demo's failed power-on, by extId: found in the store rather than pinned, so the row
/// the detail is about survives a renumbering of the fixture.
fn failed_power_on(store: &Store) -> String {
    store
        .list("/prism/v4.4/config/tasks")
        .expect("the demo has tasks")
        .iter()
        .find(|t| t["status"] == "FAILED" && t["operation"] == "VmPowerOn")
        .and_then(|t| t["extId"].as_str())
        .expect("a failed VmPowerOn task")
        .to_string()
}

macro_rules! detail_snapshot {
    ($name:ident, $fixtures:expr, $kind:expr) => {
        #[tokio::test]
        async fn $name() {
            let pc = MockPc::builder().fixtures(tree($fixtures)).start().await;
            let app = detail(&pc, $kind).await;
            let frame = app.snapshot(120, 36).unwrap();
            // The four things every composed detail must be true of.
            assert!(!frame.contains("$objectType"), "{frame}");
            assert!(!frame.contains("links:"), "{frame}");
            assert!(
                frame.contains(" · "),
                "the title is display · name: {frame}"
            );
            // Opening a pane over a row the Prism Central answered for is not an error, and a
            // kind the catalog cannot fetch singly is a fact about the catalog and not
            // something to tell the reader about in the middle of their frame.
            assert!(!frame.contains("error:"), "{frame}");
            common::settings(&pc).bind(|| {
                insta::assert_snapshot!(stringify!($name), app.snapshot(120, 36).unwrap())
            });
        }
    };
}

/// [`detail_snapshot!`] over the embedded demo estate: `$term` picks the row, from the store
/// when it must be looked up, and `$title` is what the pane's title must carry.
macro_rules! demo_detail_snapshot {
    ($name:ident, $kind:expr, $term:expr, $title:expr) => {
        #[tokio::test]
        async fn $name() {
            let store = nutsh_mockpc::demo::store(nutsh_mockpc::demo::DEMO, common::now()).unwrap();
            let term: String = ($term)(&store);
            // `honours_filter` as the demo sets it, so the header's `on/off` and alert
            // counters are the demo's and not every row counted twice.
            let pc = MockPc::builder()
                .store(store)
                .honours_filter()
                .start()
                .await;
            let app = detail_of(&pc, $kind, &term).await;
            let frame = app.snapshot(120, 36).unwrap();
            assert!(!frame.contains("$objectType"), "{frame}");
            assert!(!frame.contains("links:"), "{frame}");
            assert!(
                frame.contains($title),
                "the pane is over the story's row: {frame}"
            );
            assert!(!frame.contains("error:"), "{frame}");
            common::settings(&pc).bind(|| {
                insta::assert_snapshot!(stringify!($name), app.snapshot(120, 36).unwrap())
            });
        }
    };
}

detail_snapshot!(vm, "fixtures-lab", "vm");
detail_snapshot!(host, "fixtures-lab", "host");
detail_snapshot!(cluster, "fixtures-lab", "cluster");
detail_snapshot!(storage_container, "fixtures-lab", "storage-containers");
detail_snapshot!(subnet, "fixtures-lab", "subnet");
detail_snapshot!(alert, "fixtures-lab", "alert");
detail_snapshot!(task, "fixtures-lab", "task");
detail_snapshot!(recovery_point, "fixtures", "recovery-point");
detail_snapshot!(protection_policy, "fixtures", "protection-policies");

demo_detail_snapshot!(
    demo_vm,
    "vm",
    |_: &Store| "prd-db-01".to_string(),
    "Virtual Machines · prd-db-01"
);
demo_detail_snapshot!(
    demo_failed_task,
    "task",
    failed_power_on,
    "Tasks · VmPowerOn"
);
