//! The refresh schedule: the ladder, the marker, the verb that keeps it, and the floor.

mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::{App, Key};

/// A suffix, the way `requests_to` matches: version-independent, so a negotiation that steps
/// down does not silently make this const match nothing.
const VMS: &str = "/config/vms";

async fn app_over(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app
}

/// Run a palette verb, the way the settings tests already type into it.
fn verb(app: &mut App, line: &str) {
    app.handle(Key::Char(':'));
    for c in line.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

/// `ctrl-t` walks the ladder, the flash names the gesture that keeps it, and the marker carries
/// the interval exactly when it is no longer the catalog's - which is why the frames that were
/// already recorded do not move.
#[tokio::test]
async fn ctrl_t_walks_the_ladder_and_the_marker_says_where_it_got_to() {
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc).await;
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("● live"), "{frame}");
    assert!(!frame.contains("↻"), "the curated 5s says nothing: {frame}");

    app.handle(Key::Ctrl('t')); // 5s → 10s
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("refresh: every 10s"), "{frame}");
    assert!(
        frame.contains(":refresh 10 to keep it"),
        "the gesture that keeps it: {frame}"
    );
    assert!(
        frame.contains("● live ↻10s"),
        "not the catalog's any more: {frame}"
    );
}

/// `off`, and what `ctrl-r` then means: one cycle, and the schedule stays off.
#[tokio::test]
async fn off_reads_manual_and_ctrl_r_buys_exactly_one_cycle() {
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc).await;
    // Each named rung arms, so each of the seven buys a cycle; they are settled one at a time
    // so nothing is still on the wire when the request count below is taken. The eighth press
    // is `off`, which deliberately does not arm.
    for _ in 0..7 {
        app.handle(Key::Ctrl('t')); // 10s 30s 1m 5m 1h 6h 12h
        common::settle(&mut app).await;
    }
    app.handle(Key::Ctrl('t')); // off
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("⏹ manual"), "{frame}");
    assert!(frame.contains("refresh: off"), "{frame}");

    // Two turns of the event loop and the request count stands.
    let before = pc.requests_to(VMS).len();
    app.drain().await;
    app.drain().await;
    assert_eq!(pc.requests_to(VMS).len(), before, "nothing is polling");

    app.handle(Key::Ctrl('r'));
    common::settle(&mut app).await;
    assert!(pc.requests_to(VMS).len() > before, "one walk");
    let after = pc.requests_to(VMS).len();
    app.drain().await;
    app.drain().await;
    assert_eq!(pc.requests_to(VMS).len(), after, "exactly one");
    assert!(
        app.snapshot(120, 30).unwrap().contains("⏹ manual"),
        "and back to manual, which is the whole difference from ⏹ stopped"
    );
}

/// `:refresh` writes the file; the floor is refused rather than clamped.
#[tokio::test]
async fn the_verb_persists_and_the_floor_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "").unwrap();
    let pc = MockPc::builder().start().await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::FileContexts::at(&path)),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;

    verb(&mut app, "refresh 60");
    common::settle(&mut app).await;
    assert_eq!(
        nutsh_config::load(&path).unwrap().refresh.kinds["vmm.ahv.config.Vm"],
        nutsh_config::Interval::Secs(60)
    );
    assert!(app.snapshot(120, 30).unwrap().contains("● live ↻1m"));

    verb(&mut app, "refresh 0");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("1s is the floor"), "{frame}");
    assert_eq!(
        nutsh_config::load(&path).unwrap().refresh.kinds["vmm.ahv.config.Vm"],
        nutsh_config::Interval::Secs(60),
        "a refusal writes nothing"
    );
}
