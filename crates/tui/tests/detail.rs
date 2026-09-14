//! One frame per curated kind that has a fixture, against a real recorded shape.
//!
//! Nine of the eleven. Volume groups and images have **no fixture in either tree**; they are
//! covered by unit tests over a hand-built entity in `crates/core/tests/detail.rs` until a lab
//! recording exists, which is a limitation stated rather than papered over.

mod common;

use std::path::PathBuf;

use nutsh_mockpc::MockPc;
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

detail_snapshot!(vm, "fixtures-lab", "vm");
detail_snapshot!(host, "fixtures-lab", "host");
detail_snapshot!(cluster, "fixtures-lab", "cluster");
detail_snapshot!(storage_container, "fixtures-lab", "storage-containers");
detail_snapshot!(subnet, "fixtures-lab", "subnet");
detail_snapshot!(alert, "fixtures-lab", "alert");
detail_snapshot!(task, "fixtures-lab", "task");
detail_snapshot!(recovery_point, "fixtures", "recovery-point");
detail_snapshot!(protection_policy, "fixtures", "protection-policies");
