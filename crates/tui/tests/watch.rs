//! `w` on the detail pane: the row polled at the watch rhythm, with what moved lit.

mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key};

/// The single-entity path of the first row: what the watch polls.
const WEB01: &str = "/vms/3d0c4a2e-1b8f-4c1a-9e2f-000000000001";

async fn detail(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    common::settle_stats(&mut app).await;
    app.handle(Key::Char('y'));
    common::settle(&mut app).await;
    app
}

/// The watch polls at a rhythm the advertised tier sets, the title says so, and the value a
/// poll moved is lit where every other value is not. `w` again stops it.
#[tokio::test]
async fn a_watched_row_lights_the_value_that_moved() {
    // Thirty a second is the ceiling: a quarter of it floors at one poll a second.
    let pc = MockPc::builder().rate_limit(30, 1).start().await;
    let mut app = detail(&pc).await;
    let before = pc.requests_to(WEB01).len();

    app.handle(Key::Char('w'));
    assert_eq!(app.mode, Mode::Detail);
    assert_eq!(app.status.as_deref(), Some("watching every 1s · w stops"));
    let frame = app.snapshot(120, 36).unwrap();
    assert!(frame.contains("· watching every 1s"), "{frame}");
    common::settle(&mut app).await;
    assert!(
        pc.requests_to(WEB01).len() > before,
        "the watch asked for the row straight away"
    );

    // Power the VM off from the pane's own actions; the next watch poll sees `powerState`
    // move and lights the `Power` value.
    app.handle(Key::Char('a'));
    while app.menu_selected_action() != Some("power-off") {
        app.handle(Key::Down);
    }
    app.handle(Key::Enter);
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    assert_eq!(app.mode, Mode::Detail, "back over the pane");
    // The mock moves a task one step per Tasks poll, so the row flips a few polls in. Wait for
    // the text to say OFF, then read the colour off that same frame.
    let mut lit = false;
    for _ in 0..15 {
        common::settle(&mut app).await;
        if !app.snapshot(120, 36).unwrap().contains("Power OFF") {
            continue;
        }
        let _skin = common::pin_skin();
        let p = nutsh_tui::theme::snapshot();
        let buf = app.frame(120, 36).unwrap();
        assert_eq!(
            common::style_of(&buf, "OFF").fg,
            Some(p.peach),
            "the value the poll moved is lit"
        );
        assert_eq!(
            common::style_of(&buf, "Description").fg,
            Some(p.overlay1),
            "a label is still dim"
        );
        assert_ne!(
            common::style_of(&buf, "frontend").fg,
            Some(p.peach),
            "a value that did not move is not lit"
        );
        lit = true;
        break;
    }
    assert!(
        lit,
        "the Power value moved and lit: {}",
        app.snapshot(120, 36).unwrap()
    );

    app.handle(Key::Char('w'));
    assert_eq!(app.status.as_deref(), Some("watch off"));
    assert!(!app.snapshot(120, 36).unwrap().contains("watching"));
}

/// `esc` ends the watch with the pane: the subscription goes, and nothing polls the row at
/// the watch rhythm afterwards.
#[tokio::test]
async fn closing_the_pane_ends_the_watch() {
    let pc = MockPc::builder().rate_limit(30, 1).start().await;
    let mut app = detail(&pc).await;
    app.handle(Key::Char('w'));
    common::settle(&mut app).await;
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
    app.drain().await;
    let after = pc.requests_to(WEB01).len();
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    app.drain().await;
    assert_eq!(
        pc.requests_to(WEB01).len(),
        after,
        "nothing watches a closed pane"
    );
}
