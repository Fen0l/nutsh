//! The settings screen: what is set, what it is set to, who set it, and what a keystroke here
//! writes to the file.

mod common;

use std::path::Path;

use common::FileContexts;
use nutsh_core::contexts::Flags;
use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
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

/// `space` on a namespace row is one rung for every kind in it, kept in `[refresh.namespaces]`,
/// and the kinds underneath read the namespace as their source until one of them is set alone.
#[tokio::test]
async fn a_namespace_row_schedules_all_its_kinds_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    open_settings(&mut app);
    select(&mut app, "▸ vmm");
    app.handle(Key::Char(' '));
    assert_eq!(
        app.status.as_deref(),
        Some("vmm: 5s"),
        "one rung on from auto"
    );
    let cfg = nutsh_config::load(&path).unwrap();
    assert_eq!(
        cfg.refresh.namespaces["vmm"],
        nutsh_config::Interval::Secs(5)
    );
    assert!(
        cfg.refresh.kinds.is_empty(),
        "no kind was written on its own"
    );

    select(&mut app, "▸ vmm");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("vmm") && l.contains("5s") && l.contains("namespace vmm")),
        "{frame}"
    );
    app.handle(Key::Enter);
    select(&mut app, "Virtual Machines");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame.lines().any(|l| l.contains("Virtual Machines")
            && l.contains("5s")
            && l.contains("namespace vmm")),
        "the kind polls at the namespace's rhythm: {frame}"
    );
    // The VM table is open and has polled; its siblings have not, and say so in a word.
    let vm = frame
        .lines()
        .find(|l| l.contains("Virtual Machines"))
        .unwrap();
    assert!(!vm.contains("never"), "{vm}");
    assert!(
        frame
            .lines()
            .any(|l| l.contains("namespace vmm") && l.contains("never")),
        "a kind no table is open on reads `never`, not `-`: {frame}"
    );

    // A second press walks the ladder from the namespace's own value, not from a kind's.
    select(&mut app, "▾ vmm");
    app.handle(Key::Char(' '));
    let cfg = nutsh_config::load(&path).unwrap();
    assert_eq!(
        cfg.refresh.namespaces["vmm"],
        nutsh_config::Interval::Secs(10)
    );
}

/// The run row at the top of the section asks every subscription for a cycle now, and says how
/// many it asked.
#[tokio::test]
async fn refresh_everything_now_repolls_the_open_view() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;
    common::settle_stats(&mut app).await;
    let before = pc.requests_to("/config/vms").len();

    open_settings(&mut app);
    select(&mut app, "⏎ refresh everything now");
    app.handle(Key::Enter);
    let status = app.status.clone().unwrap_or_default();
    assert!(status.starts_with("refreshing "), "{status}");
    assert_ne!(status, "refreshing 0 views", "{status}");

    common::settle(&mut app).await;
    assert!(
        pc.requests_to("/config/vms").len() > before,
        "the VM table was asked again"
    );
    assert_eq!(app.mode, Mode::Settings, "the screen stays open");
}

/// The hour rungs are real settings: `:refresh 6h` writes seconds, and the screen reads it back
/// in hours from the file.
#[tokio::test]
async fn an_hour_rung_writes_seconds_and_reads_back_in_hours() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    app.handle(Key::Char(':'));
    for c in "refresh 6h".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert_eq!(
        app.status.as_deref(),
        Some("refresh: 6h kept for Virtual Machines")
    );
    let cfg = nutsh_config::load(&path).unwrap();
    assert_eq!(
        cfg.refresh.kinds["vmm.ahv.config.Vm"],
        nutsh_config::Interval::Secs(21_600)
    );

    open_settings(&mut app);
    select(&mut app, "▸ vmm");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame.lines().any(|l| l.contains("vmm (1 set)")),
        "the folded namespace counts the kind set on its own: {frame}"
    );
    app.handle(Key::Enter);
    select(&mut app, "Virtual Machines");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame.lines().any(|l| l.contains("Virtual Machines")
            && l.contains("6h")
            && l.contains("config file")),
        "{frame}"
    );
}

const RUNGS: [&str; 10] = [
    "auto", "5s", "10s", "30s", "1m", "5m", "1h", "6h", "12h", "off",
];

/// `⏎` on a kind's row shows the whole ladder with the rung in force marked; a choice is
/// written the way `space` writes one, and `esc` writes nothing.
#[tokio::test]
async fn enter_on_a_refresh_row_opens_the_ladder_and_a_choice_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let pc = MockPc::builder().start().await;
    let mut app = app_over(&pc, &path).await;

    open_settings(&mut app);
    select(&mut app, "▸ vmm");
    app.handle(Key::Enter);
    select(&mut app, "Virtual Machines");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Ladder);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains(" refresh · Virtual Machines "), "{frame}");
    for rung in ["auto", "5s", "1h", "12h", "off"] {
        assert!(frame.contains(rung), "{rung} is on the list: {frame}");
    }
    assert!(
        RUNGS.iter().all(|r| !frame.contains(&format!("{r} •"))),
        "the catalog's 5s is not a rung in the file, so none is marked: {frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Settings);
    assert!(
        nutsh_config::load(&path).unwrap().refresh.kinds.is_empty(),
        "esc wrote nothing"
    );

    // Opens at the top, so four steps down is `1m`.
    select(&mut app, "Virtual Machines");
    app.handle(Key::Enter);
    for _ in 0..4 {
        app.handle(Key::Char('j'));
    }
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Settings);
    assert_eq!(app.status.as_deref(), Some("refresh: 1m"));
    assert_eq!(
        nutsh_config::load(&path).unwrap().refresh.kinds["vmm.ahv.config.Vm"],
        nutsh_config::Interval::Secs(60)
    );
    select(&mut app, "Virtual Machines");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame.lines().any(|l| l.contains("Virtual Machines")
            && l.contains("1m")
            && l.contains("config file")),
        "{frame}"
    );
    // Reopened, the list marks the rung the file now holds.
    app.handle(Key::Enter);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.lines().any(|l| l.contains("1m •")), "{frame}");
    app.handle(Key::Esc);

    // The global row goes through the same list and lands in `[refresh] default`.
    select(&mut app, "every kind");
    app.handle(Key::Enter);
    assert!(
        app.snapshot(120, 30)
            .unwrap()
            .contains(" refresh · every kind ")
    );
    app.handle(Key::Char('G'));
    app.handle(Key::Enter);
    assert_eq!(
        nutsh_config::load(&path).unwrap().refresh.default,
        Some(nutsh_config::Interval::Word(nutsh_config::Word::Off))
    );
}
