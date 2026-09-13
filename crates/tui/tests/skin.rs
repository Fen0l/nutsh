mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key, theme};

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

/// `:` then a command line then `enter`.
fn command(app: &mut App, line: &str) {
    app.handle(Key::Char(':'));
    for c in line.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

/// `:skin NAME` applies without a restart - §1's exit criterion - and says so.
#[tokio::test]
async fn skin_with_a_name_applies_live() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    command(&mut app, "skin gruvbox-dark");
    assert_eq!(app.mode, Mode::Table, "no box for a named skin");
    assert_eq!(
        theme::snapshot().base,
        theme::builtin("gruvbox-dark").unwrap().base
    );
    assert_eq!(app.status.as_deref(), Some("skin gruvbox-dark"));

    // A name nothing knows is a status line, not a refusal, and the skin is kept.
    command(&mut app, "skin no-such-skin");
    assert!(
        app.status
            .as_deref()
            .unwrap()
            .contains("unknown skin 'no-such-skin'"),
        "{:?}",
        app.status
    );
    assert_eq!(
        theme::snapshot().base,
        theme::builtin("gruvbox-dark").unwrap().base
    );
}

/// An alias applies the same skin and is reported, recorded and marked under its canonical
/// name, so bare `:skin` afterwards opens on the row that is showing.
#[tokio::test]
async fn an_alias_is_recorded_under_its_canonical_name() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    command(&mut app, "skin Gruvbox");
    assert_eq!(app.status.as_deref(), Some("skin gruvbox-dark"));
    assert_eq!(theme::current_name(), "gruvbox-dark");

    command(&mut app, "skin");
    assert_eq!(app.mode, Mode::Skins);
    assert_eq!(
        app.skins.as_ref().unwrap().selected,
        4,
        "gruvbox-dark is the fifth built-in"
    );
    let frame = app.snapshot(80, 24).unwrap();
    assert!(frame.contains("▌ gruvbox-dark •"), "{frame}");
    assert!(!frame.contains("catppuccin-mocha •"), "{frame}");
}

/// Bare `:skin` lists the built-ins with the current one selected; `enter` applies; `esc` and
/// `enter` both go back to the screen the list was opened over, which is the Contexts screen
/// as readily as a table.
#[tokio::test]
async fn bare_skin_opens_the_list() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    command(&mut app, "skin");
    assert_eq!(app.mode, Mode::Skins);
    let frame = app.snapshot(80, 24).unwrap();
    assert!(frame.contains("gruvbox-dark"), "{frame}");
    assert!(frame.contains("▌ catppuccin-mocha •"), "{frame}");
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);

    command(&mut app, "skin");
    app.handle(Key::Down);
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(app.status.as_deref(), Some("skin catppuccin-latte"));

    // Opened over the Contexts screen: drawn over it, and closed back onto it.
    command(&mut app, "ctx");
    assert_eq!(app.mode, Mode::Contexts);
    command(&mut app, "skin");
    assert_eq!(app.mode, Mode::Skins);
    let frame = app.snapshot(80, 24).unwrap();
    assert!(
        !frame.contains("POWER"),
        "the list is over the Contexts screen, not the table:\n{frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Contexts);
}

/// `:skin onedark` completes: the alias matches and the row offered is the canonical name, which
/// is the spelling `enter` applies, the config records and the list marks. Before, the popup
/// matched only the seventeen canonical spellings, so `onedark` showed the bare `:skin` row -
/// a list that lied about what it accepts, since `enter` on it worked all along.
#[tokio::test]
async fn an_alias_completes_under_its_canonical_name() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    app.handle(Key::Char(':'));
    for c in "skin onedark".chars() {
        app.handle(Key::Char(c));
    }
    let frame = app.snapshot(100, 24).unwrap();
    assert!(frame.contains("one-dark"), "the canonical row: {frame}");
    assert!(
        !frame.contains(":skin  "),
        "not the bare command row: {frame}"
    );
    assert!(frame.contains(" skins "), "titled for the slot: {frame}");
    assert!(
        frame.contains("⇥ complete"),
        "and for what tab now does: {frame}"
    );
    app.handle(Key::Enter);
    assert_eq!(app.status.as_deref(), Some("skin one-dark"));
    assert_eq!(theme::current_name(), "one-dark");
}
