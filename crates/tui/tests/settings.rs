//! The settings screen: what is set, what it is set to, who set it, and what a keystroke here
//! writes to the file.

mod common;

use std::path::Path;

use common::FileContexts;
use nutsh_core::contexts::Flags;
use nutsh_mockpc::MockPc;
use nutsh_tui::{App, Key};

const FILE: &str = "[skin]\nname = \"catppuccin-mocha\"\n\n\
                    [nav]\nhide = [\"vmm.esxi.config.Vm\", \"Data Protection\"]\n";

async fn app_over(pc: &MockPc, path: &Path) -> App {
    std::fs::write(path, FILE).unwrap();
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        // This run was started with `--readonly`, which is the provenance no file carries.
        Box::new(FileContexts::at(path).with_flags(Flags {
            readonly: true,
            ..Flags::default()
        })),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app
}

/// Move the cursor to the row that *starts* with `label`, anchored on the cursor mark: there is
/// one row per kind now, and `Virtual Machines` is inside `ESXi Virtual Machines`. The movement
/// is the screen's own, which is the honest way to reach a row; a hard-coded count would break
/// the day one is added between.
fn select(app: &mut App, label: &str) {
    app.handle(Key::Char('g'));
    for _ in 0..260 {
        let frame = app.snapshot(120, 30).unwrap();
        if frame.lines().any(|l| {
            l.split('▌')
                .nth(1)
                .is_some_and(|rest| rest.trim_start().starts_with(label))
        }) {
            return;
        }
        app.handle(Key::Char('j'));
    }
    panic!("no {label} row:\n{}", app.snapshot(120, 30).unwrap());
}

fn open_settings(app: &mut App) {
    app.handle(Key::Char(':'));
    for c in "settings".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

/// Every setting this build has, its value, and where that value came from - including the two
/// the screen can only show.
#[tokio::test]
async fn the_screen_says_what_is_set_and_who_set_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    open_settings(&mut app);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains(" settings "), "the popup's title: {frame}");
    assert!(frame.contains("skin"), "{frame}");
    assert!(
        frame.contains("catppuccin-mocha"),
        "the skin on screen: {frame}"
    );
    assert!(frame.contains("cache"), "{frame}");
    assert!(frame.contains("mouse"), "{frame}");
    assert!(frame.contains("header"), "{frame}");
    assert!(frame.contains("guardrails"), "{frame}");
    assert!(frame.contains("hide unserved"), "{frame}");
    assert!(
        frame.contains("default"),
        "a value nobody set says so: {frame}"
    );
    assert!(
        frame.contains("config file"),
        "and one the file set says so: {frame}"
    );
    assert!(
        frame.contains("read-only") && frame.contains("--readonly"),
        "a flag is provenance the file cannot carry: {frame}"
    );
    // The hide list is on the same screen, one row per entry.
    assert!(frame.contains("vmm.esxi.config.Vm"), "{frame}");
    assert!(frame.contains("Data Protection"), "{frame}");
}

/// The toggle this whole screen exists for: `hide unserved` off by default, on by a keystroke,
/// and the menu behind it changes on that same keystroke.
#[tokio::test]
async fn toggling_hide_unserved_reaches_the_menu_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder()
        .unavailable_namespace("volumes")
        .start()
        .await;
    let mut app = app_over(&pc, &path).await;
    app.handle(Key::Char('2')); // Compute & Storage
    app.handle(Key::Char('l')); // expand
    assert!(
        app.snapshot(120, 30).unwrap().contains("Volume Groups"),
        "greyed, not gone, until asked"
    );

    open_settings(&mut app);
    select(&mut app, "hide unserved");
    app.handle(Key::Char(' '));
    app.handle(Key::Esc);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(!frame.contains("Volume Groups"), "{frame}");
    assert!(frame.contains("hidden)"), "{frame}");
    assert!(
        nutsh_config::load(&path).unwrap().nav.hide_unserved,
        "written"
    );

    // And back off, which is the half a one-way door would not have.
    open_settings(&mut app);
    select(&mut app, "hide unserved");
    app.handle(Key::Char(' '));
    app.handle(Key::Esc);
    assert!(app.snapshot(120, 30).unwrap().contains("Volume Groups"));
    let cfg = nutsh_config::load(&path).unwrap();
    assert!(!cfg.nav.hide_unserved);
    assert_eq!(
        cfg.skin.name.as_deref(),
        Some("catppuccin-mocha"),
        "no other section moved"
    );
}

/// A setting the screen cannot honestly change is shown, read-only, with its reason - and
/// writes nothing.
#[tokio::test]
async fn a_fixed_setting_explains_itself_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    open_settings(&mut app);
    select(&mut app, "read-only");
    app.handle(Key::Char(' '));
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("--readonly wins for this run"), "{frame}");
    assert!(
        !nutsh_config::load(&path).unwrap().readonly,
        "nothing was written"
    );
}

/// `mouse` and `header` are written by the commands that already own them, so the screen never
/// becomes a second writer for a setting the palette also sets.
#[tokio::test]
async fn mouse_and_header_go_through_the_commands_that_own_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    open_settings(&mut app);
    select(&mut app, "mouse");
    app.handle(Key::Char(' '));
    assert!(!nutsh_config::load(&path).unwrap().mouse, "mouse off");
    assert!(!app.mouse_enabled(), "and off on the screen too");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains(" settings "), "still open: {frame}");
    assert!(
        frame.contains("off"),
        "the row shows the new value: {frame}"
    );

    // `:header` cycles auto, compact, full, and this run starts on full, so one press is auto
    // - which a file that never mentioned the key already means - and the second is compact.
    select(&mut app, "header");
    app.handle(Key::Char(' '));
    assert_eq!(
        nutsh_config::load(&path).unwrap().header,
        nutsh_config::Header::Auto
    );
    select(&mut app, "header");
    app.handle(Key::Char(' '));
    assert_eq!(
        nutsh_config::load(&path).unwrap().header,
        nutsh_config::Header::Compact
    );
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("header") && l.contains("compact")),
        "the row shows what the file now says: {frame}"
    );
}

/// `d` on a hide entry is a `:show`: that entry goes, the rest of the list stays, and the
/// file's other sections do not move - which is what a whole-file atomic rewrite guarantees.
#[tokio::test]
async fn d_removes_one_hide_entry_and_a_opens_the_palette_on_hide() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    open_settings(&mut app);
    select(&mut app, "Data Protection");
    app.handle(Key::Char('d'));
    let cfg = nutsh_config::load(&path).unwrap();
    assert_eq!(cfg.nav.hide, ["vmm.esxi.config.Vm"]);
    assert_eq!(cfg.skin.name.as_deref(), Some("catppuccin-mocha"));

    // `a` types the verb the palette already completes, rather than growing a second list.
    app.handle(Key::Char('a'));
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame.contains(":hide "),
        "the palette, on the hide verb: {frame}"
    );
}

/// The two schedule rows: the global default, and the kind in front of the user with where its
/// value came from. `space` on the per-kind row keeps the next rung in `[refresh.kinds]` and
/// the source column says so on the very next frame.
#[tokio::test]
async fn the_schedule_rows_show_their_provenance_and_space_keeps_the_next_rung() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    open_settings(&mut app);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("every kind") && l.contains("auto") && l.contains("default")),
        "the global default, untouched: {frame}"
    );
    // The kinds are folded under their namespace, so `vmm` is opened before its rows exist.
    select(&mut app, "▸ vmm");
    app.handle(Key::Enter);
    select(&mut app, "Virtual Machines");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("Virtual Machines") && l.contains("catalog")),
        "the kind's own row, on the catalog's rhythm: {frame}"
    );

    // `ctrl-t` on the view is this session's, and the row says so.
    app.handle(Key::Esc);
    app.handle(Key::Ctrl('t'));
    open_settings(&mut app);
    // The fold survives a reopen, so `vmm` is still the way it was left.
    select(&mut app, "Virtual Machines");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("Virtual Machines") && l.contains("this session")),
        "{frame}"
    );

    // `space` writes it, and the row reads `config file` on the next frame.
    select(&mut app, "Virtual Machines");
    app.handle(Key::Char(' '));
    let cfg = nutsh_config::load(&path).unwrap();
    assert_eq!(
        cfg.refresh.kinds["vmm.ahv.config.Vm"],
        nutsh_config::Interval::Secs(30),
        "one rung on from the 10s ctrl-t left"
    );
    assert_eq!(
        cfg.skin.name.as_deref(),
        Some("catppuccin-mocha"),
        "no other section moved"
    );
    // The write reopens the screen, which puts the cursor back at the top. The fold survives,
    // so `vmm` is still open.
    select(&mut app, "Virtual Machines");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("Virtual Machines") && l.contains("config file")),
        "{frame}"
    );
}
