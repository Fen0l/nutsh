mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::Focus;
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
    app
}

/// What the fake seam wrote, or `None` when `:hide`/`:show` never reached it. The trait's own
/// default is a no-op, so a fake that keeps it proves nothing about what was persisted.
type Saved = std::sync::Arc<std::sync::Mutex<Option<Vec<String>>>>;

/// An app whose contexts seam answers a `[nav]` section, without a config file.
async fn app_with_nav(pc: &MockPc, hide: &[&str], hide_unserved: bool) -> App {
    app_recording_nav(pc, hide, hide_unserved).await.0
}

/// The same app, plus the list its seam was last asked to persist.
async fn app_recording_nav(pc: &MockPc, hide: &[&str], hide_unserved: bool) -> (App, Saved) {
    struct Hidden(Vec<String>, bool, Saved);
    impl nutsh_core::contexts::Contexts for Hidden {
        fn list(&self) -> anyhow::Result<Vec<nutsh_core::contexts::ContextRow>> {
            Ok(Vec::new())
        }
        fn connect(
            &self,
            _req: nutsh_core::contexts::ConnectRequest,
        ) -> futures::future::BoxFuture<'static, anyhow::Result<nutsh_core::contexts::Connected>>
        {
            Box::pin(async { anyhow::bail!("no contexts in this test") })
        }
        fn remove(&self, _name: &str) -> anyhow::Result<Vec<String>> {
            anyhow::bail!("no contexts in this test")
        }
        fn nav_hidden(&self) -> Vec<String> {
            self.0.clone()
        }
        fn nav_hide_unserved(&self) -> bool {
            self.1
        }
        fn set_nav_hidden(&self, hide: &[String]) -> anyhow::Result<()> {
            *self.2.lock().unwrap() = Some(hide.to_vec());
            Ok(())
        }
    }
    let saved: Saved = Saved::default();
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(Hidden(
            hide.iter().map(|s| (*s).to_string()).collect(),
            hide_unserved,
            saved.clone(),
        )),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    (app, saved)
}

/// `NAV`'s index for a group spelled exactly this way. Exact, because the two bugs below are
/// both about a name matched loosely and then compared strictly.
fn group_index(name: &str) -> usize {
    nutsh_catalog::NAV
        .iter()
        .position(|g| g.name == name)
        .unwrap_or_else(|| panic!("no group named {name}"))
}

/// Type `text` into the palette and run it.
fn run(app: &mut App, text: &str) {
    app.handle(Key::Char(':'));
    for c in text.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

/// The menu is beside the body, not full height: the header describes the session, and the
/// session is above the navigation, not beside it.
#[tokio::test]
async fn the_sidebar_sits_beside_the_body_and_toggles() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("▸ Dashboard"), "{frame}");
    assert!(
        frame.contains("▾ Compute & Storage"),
        "the open kind's group is expanded: {frame}"
    );
    assert!(frame.contains("• VMs"), "the open item is marked: {frame}");
    assert!(
        frame.lines().next().unwrap().starts_with("╭ nutsh "),
        "the header is full width: {frame}"
    );

    app.handle(Key::Ctrl('b'));
    let hidden = app.snapshot(100, 30).unwrap();
    assert!(!hidden.contains("▸ Dashboard"), "{hidden}");
    app.handle(Key::Ctrl('b'));
    assert!(app.snapshot(100, 30).unwrap().contains("▸ Dashboard"));
}

/// Under 90 columns a split would leave a 66-cell table, so the menu waits to be asked for -
/// and `ctrl-b` flips what is on the frame, not the stored wish, so the first press is the one
/// that reveals it. The header and the prompt line advertise `^b menu`: it cannot be a no-op
/// at the width where the menu is most needed.
#[tokio::test]
async fn one_ctrl_b_reveals_the_overlay_on_a_narrow_frame() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let narrow = app.snapshot(80, 30).unwrap();
    assert!(!narrow.contains("▸ Dashboard"), "{narrow}");

    app.handle(Key::Ctrl('b'));
    let overlaid = app.snapshot(80, 30).unwrap();
    assert!(overlaid.contains("▸ Dashboard"), "{overlaid}");
    // Over the body, not beside it: the table's own border is behind the menu's.
    assert!(overlaid.contains("▸ Dashboard         │"), "{overlaid}");

    app.handle(Key::Ctrl('b'));
    assert!(!app.snapshot(80, 30).unwrap().contains("▸ Dashboard"));
}

/// `tab` moves the keys into the menu only when the menu is on the frame: a narrow terminal
/// that has not asked for one would otherwise swallow `j`/`k`/`enter` into a pane nobody can
/// see.
#[tokio::test]
async fn tab_does_nothing_while_the_menu_is_not_drawn() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let narrow = app.snapshot(80, 30).unwrap();
    assert!(!narrow.contains("▸ Dashboard"), "{narrow}");
    app.handle(Key::Tab);
    assert_eq!(app.focus, Focus::Body, "the keys stay with the table");
    // And once it is drawn, `tab` reaches it.
    app.handle(Key::Ctrl('b'));
    assert!(app.snapshot(80, 30).unwrap().contains("▸ Dashboard"));
    app.handle(Key::Tab);
    assert_eq!(app.focus, Focus::Sidebar);
}

/// The menu binds a dozen keys; the rest still belong to the body, which is what the prompt
/// line under the menu goes on advertising.
#[tokio::test]
async fn the_body_keys_still_work_with_the_menu_focused() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Tab);
    assert_eq!(app.focus, Focus::Sidebar);
    app.handle(Key::Char(':'));
    assert_eq!(app.mode, nutsh_tui::app::Mode::Command);
    app.handle(Key::Esc);
    app.handle(Key::Char('?'));
    assert_eq!(app.mode, nutsh_tui::app::Mode::Help);
}

#[tokio::test]
async fn tab_moves_focus_and_enter_opens() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    assert_eq!(app.focus, Focus::Body);
    app.handle(Key::Tab);
    assert_eq!(app.focus, Focus::Sidebar);
    // `5` jumps to the fifth curated group, Hardware, and `enter` on Clusters opens it. Two
    // `j`s, not one: the group's first item is its landing page and Clusters is the second.
    app.handle(Key::Char('5'));
    app.handle(Key::Char('l'));
    app.handle(Key::Char('j'));
    app.handle(Key::Char('j'));
    app.handle(Key::Enter);
    common::settle(&mut app).await;
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "clustermgmt.config.Cluster"
    );
    assert_eq!(
        app.focus,
        Focus::Body,
        "opening returns the keys to the body"
    );
    let frame = app.snapshot(100, 30).unwrap();
    assert!(
        frame.contains("Hardware › Clusters"),
        "the breadcrumb: {frame}"
    );
    assert!(frame.contains("• Clusters"), "{frame}");
}

/// A `Missing` item says why, and `enter` on it puts the reason on the status line rather than
/// opening an empty table.
#[tokio::test]
async fn a_missing_item_is_greyed_and_explains() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Tab);
    app.handle(Key::Char('2')); // Compute & Storage
    app.handle(Key::Char('l'));
    // Thirteen, not twelve: the group's first item is its landing page and `Catalog Items` is
    // the last of the thirteen under the header.
    for _ in 0..13 {
        app.handle(Key::Char('j'));
    }
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("Catalog Items"), "{frame}");
    app.handle(Key::Enter);
    assert_eq!(
        app.status.as_deref(),
        Some("Catalog Items: not in the v4 API")
    );
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "vmm.ahv.config.Vm",
        "nothing opened"
    );
}

/// `/` filters the labels; `esc` clears it.
#[tokio::test]
async fn the_sidebar_filters_by_substring() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Tab);
    app.handle(Key::Char('/'));
    for c in "recovery".chars() {
        app.handle(Key::Char(c));
    }
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("Recovery Plans"), "{frame}");
    assert!(!frame.contains("Subnets"), "{frame}");
    app.handle(Key::Esc);
    assert!(
        app.snapshot(100, 30)
            .unwrap()
            .contains("▸ Network & Security")
    );
}

/// The palette is the fast path and the menu is the discoverable one; opening from either
/// highlights the same item.
#[tokio::test]
async fn a_palette_jump_moves_the_menus_marker() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    for c in "subnet".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("• Subnets"), "{frame}");
    assert!(frame.contains("▾ Network & Security"), "{frame}");
}

/// `esc` is a toggle between the menu and the body, exactly as `tab` is, and pressing it twice
/// returns you where you started.
#[tokio::test]
async fn esc_on_a_root_table_focuses_the_menu_and_back() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    assert_eq!(app.focus, Focus::Body);
    app.handle(Key::Esc);
    assert_eq!(app.focus, Focus::Sidebar, "a root table has nothing to pop");
    app.handle(Key::Esc);
    assert_eq!(app.focus, Focus::Body, "and back");
}

/// A key that advertises "take me to the menu" must not be a no-op at the width where the menu
/// is not drawn: the digits already set `visible`, `toggled` and the focus together, and `esc`
/// goes through the same helper so the two paths cannot diverge.
#[tokio::test]
async fn esc_shows_a_hidden_menu_before_focusing_it() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Ctrl('b'));
    let hidden = app.snapshot(100, 30).unwrap();
    assert!(!hidden.contains("▸ Dashboard"), "{hidden}");
    app.handle(Key::Esc);
    let shown = app.snapshot(100, 30).unwrap();
    assert!(shown.contains("▸ Dashboard"), "{shown}");
    assert_eq!(app.focus, Focus::Sidebar);
}

/// Rule 5 above rule 6: a drilled-in view pops, and only the root goes to the menu.
#[tokio::test]
async fn esc_below_the_root_still_pops() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Enter); // the picker over the selected VM
    for c in "disk".chars() {
        app.handle(Key::Char(c)); // the picker filters; the first child is Cd Roms
    }
    app.handle(Key::Enter); // the disks table
    common::settle(&mut app).await;
    let drilled = app.snapshot(120, 20).unwrap();
    assert!(drilled.contains("Disks"), "{drilled}");
    app.handle(Key::Esc);
    let popped = app.snapshot(120, 20).unwrap();
    assert!(popped.contains("Virtual Machines"), "{popped}");
    assert_eq!(app.focus, Focus::Body, "a pop does not move the focus");
}

/// Rule 2 and rule 3 are the sidebar's own and are unchanged: `/` filtering swallows `esc`
/// once to clear the filter, and the next one blurs.
#[tokio::test]
async fn esc_clears_the_filter_before_it_blurs() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Esc);
    assert_eq!(app.focus, Focus::Sidebar);
    app.handle(Key::Char('/'));
    app.handle(Key::Char('v'));
    app.handle(Key::Esc);
    assert_eq!(
        app.focus,
        Focus::Sidebar,
        "the filter went, the focus stayed"
    );
    app.handle(Key::Esc);
    assert_eq!(app.focus, Focus::Body);
}

/// The prompt line says what the key will do, which on a root view is "the menu".
#[tokio::test]
async fn the_prompt_says_what_esc_does_here() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    assert!(
        app.snapshot(120, 20).unwrap().contains("esc:menu"),
        "a root table"
    );
    app.handle(Key::Enter);
    app.handle(Key::Enter);
    common::settle(&mut app).await;
    assert!(
        app.snapshot(120, 20).unwrap().contains("esc:back"),
        "a drilled-in table"
    );
}

/// The default, and the rule this task exists to state: a namespace the Prism Central answered
/// at no version stays in the menu, greyed, and `enter` on it says why - the same sentence the
/// palette shows beside its own greyed entry and `open_root` refuses with. Nothing is hidden
/// unless the user asked for it, so no header counts anything.
#[tokio::test]
async fn a_namespace_the_pc_does_not_serve_is_greyed_and_stays_in_the_menu() {
    let pc = MockPc::builder()
        .unavailable_namespace("volumes")
        .start()
        .await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('2')); // Compute & Storage
    app.handle(Key::Char('l')); // expand
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("Volume Groups"), "greyed, not gone: {frame}");
    assert!(
        !frame.contains("hidden)"),
        "nothing was asked to be hidden, so nothing was dropped: {frame}"
    );

    // `enter` on it explains rather than opening: the reason is the one string three surfaces
    // share. `↓` and not `j`, which the filter is taking as a character.
    app.handle(Key::Char('/'));
    for c in "volume".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Down); // past the group header, onto the item
    app.handle(Key::Enter);
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("namespace not served"), "{frame}");
}

/// The setting, which is the only thing that turns greying into hiding: `[nav] hide_unserved`.
/// The group keeps its header and says how many it dropped - nothing is ever silently gone,
/// which is the whole risk of the feature the user has to opt into.
#[tokio::test]
async fn hide_unserved_drops_those_kinds_and_the_header_counts_them() {
    let pc = MockPc::builder()
        .unavailable_namespace("volumes")
        .start()
        .await;
    let mut app = app_with_nav(&pc, &[], true).await;
    app.handle(Key::Char('2')); // Compute & Storage
    app.handle(Key::Char('l')); // expand
    let frame = app.snapshot(100, 30).unwrap();
    assert!(!frame.contains("Volume Groups"), "{frame}");
    assert!(frame.contains("(1 hidden)"), "{frame}");
    // The item under the one that went, at the widest label a 24-cell menu draws whole.
    assert!(
        frame.contains("Storage Policies"),
        "the rest of the group stays: {frame}"
    );

    // The filter always matches hidden items and draws them dim: a search never lies about the
    // catalog, so if a kind exists, typing its name finds it.
    app.handle(Key::Char('/'));
    for c in "volume".chars() {
        app.handle(Key::Char(c));
    }
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("Volume Groups"), "{frame}");

    // `:all` suspends rule 2's dropping for the session and says so in the sidebar's title.
    app.handle(Key::Esc);
    app.handle(Key::Char(':'));
    for c in "all".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    let frame = app.snapshot(100, 30).unwrap();
    assert!(frame.contains("Volume Groups"), "{frame}");
    assert!(frame.contains("(all)"), "{frame}");
}

/// Rule 1 outranks rule 2 in both directions: an id or a group name in `[nav] hide` is hidden
/// even when its namespace is served, because the user said so, and `:all` does not lift it -
/// an explicit hide is an instruction, not a heuristic. It needs no `hide_unserved`.
#[tokio::test]
async fn an_explicit_hide_removes_a_whole_group_and_survives_all() {
    let pc = MockPc::builder().start().await;
    let mut app = app_with_nav(&pc, &["Data Protection"], false).await;
    let frame = app.snapshot(100, 30).unwrap();
    assert!(!frame.contains("Data Protection"), "{frame}");
    app.handle(Key::Char(':'));
    for c in "all".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert!(
        !app.snapshot(100, 30).unwrap().contains("Data Protection"),
        "an explicit hide is an instruction, not a heuristic"
    );

    // `:show` lifts it and persists; `:hide` puts it back.
    app.handle(Key::Char(':'));
    for c in "show Data Protection".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert!(app.snapshot(100, 30).unwrap().contains("Data Protection"));
}

/// What this deliberately does **not** do, and the re-anchoring rule.
#[tokio::test]
async fn nothing_hides_on_a_zero_count_and_the_cursor_never_dangles() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    // `vmm.esxi.config.Vm` has no fixture, so the mock answers an empty list: a completed cycle
    // with a count of zero. It stays in the menu.
    app.handle(Key::Char(':'));
    for c in "esxi".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    common::settle(&mut app).await;
    assert!(app.snapshot(100, 30).unwrap().contains("ESXi VMs"));

    // The item under the cursor is never removed by a `:hide` naming its group: the cursor lands
    // on that group's header instead.
    app.handle(Key::Char(':'));
    for c in "hide Compute & Storage".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    let frame = app.snapshot(100, 30).unwrap();
    assert!(
        frame.contains("ESXi VMs"),
        "the open item is never hidden - hiding the view under the cursor is a trap: {frame}"
    );
}

/// The Dashboard is the way back to a screen that works, so neither spelling may hide it: not
/// the group name the menu draws, and not the page id behind it.
///
/// Two bugs in one guard. `nav_id` resolves a kind or a page **exactly** and a group
/// **case-insensitively**, and the catalog holds page `dashboard` beside group `Dashboard`, so
/// `:hide Dashboard` reached the guard - which cancelled the hide but still reported success and
/// persisted a dead entry to the config file - while `:hide dashboard` resolved to the page id
/// and walked straight past it, leaving the group a header with nothing under it and the
/// Dashboard unreachable.
#[tokio::test]
async fn the_dashboard_cannot_be_hidden_by_either_spelling() {
    let pc = MockPc::builder().start().await;
    let dashboard = group_index("Dashboard");
    for spelling in ["dashboard", "Dashboard"] {
        let (mut app, saved) = app_recording_nav(&pc, &[], false).await;
        run(&mut app, &format!("hide {spelling}"));
        assert!(
            !app.sidebar.is_hidden(dashboard, 0),
            "`:hide {spelling}` hid it"
        );
        assert_eq!(
            app.status.as_deref(),
            Some("the Dashboard cannot be hidden"),
            "`:hide {spelling}` reported the wrong thing"
        );
        assert_eq!(
            *saved.lock().unwrap(),
            None,
            "`:hide {spelling}` persisted a dead entry"
        );
    }
}

/// `NAV` holds the curated `Monitoring` beside the generated `monitoring`, and only an exact
/// match can tell them apart. Resolving the name case-insensitively to a canonical string and
/// then comparing that string exactly always found the curated group first and could never hide
/// the generated one - so `:hide monitoring` silently hid something else.
#[tokio::test]
async fn a_group_is_hidden_by_its_own_spelling_and_not_its_twins() {
    let pc = MockPc::builder().start().await;
    let curated = group_index("Monitoring");
    let generated = group_index("monitoring");
    let app = app_with_nav(&pc, &["monitoring"], false).await;
    assert!(app.sidebar.is_hidden(generated, 0), "the generated group");
    assert!(!app.sidebar.is_hidden(curated, 0), "the curated group");
    let app = app_with_nav(&pc, &["Monitoring"], false).await;
    assert!(app.sidebar.is_hidden(curated, 0), "the curated group");
    assert!(!app.sidebar.is_hidden(generated, 0), "the generated group");
}

/// `:all` suspends rule 2 and nothing else, so it may not say it is showing every kind while an
/// explicit hide keeps one out - in a menu whose whole thesis is that it never lies about the
/// catalog. The title said `(all)` and the status line said `showing every kind this session`
/// with a group hidden on screen.
#[tokio::test]
async fn all_does_not_claim_what_an_explicit_hide_keeps_hidden() {
    let pc = MockPc::builder().start().await;
    let mut app = app_with_nav(&pc, &["Data Protection"], true).await;
    run(&mut app, "all");
    let frame = app.snapshot(100, 30).unwrap();
    assert!(!frame.contains("(all)"), "the title overclaims: {frame}");
    assert!(frame.contains("(all but 1)"), "{frame}");
    assert_eq!(
        app.status.as_deref(),
        Some("showing every kind this session except the 1 you hid")
    );
}

/// A group hidden whole leaves no header, no count and nothing on screen - by design - so the
/// only way back is to name it, and the name is exactly what a user who hid it last week does
/// not have. `:show ` therefore completes from what is hidden, and `:hide ` from what the menu
/// holds, rather than either offering no row at all.
#[tokio::test]
async fn hide_and_show_complete_the_names_they_take() {
    let pc = MockPc::builder().start().await;
    let (mut app, saved) = app_recording_nav(&pc, &["Data Protection"], false).await;
    assert!(
        !app.snapshot(100, 30).unwrap().contains("Data Protection"),
        "the group is hidden, so its name is nowhere on screen"
    );
    app.handle(Key::Char(':'));
    for c in "show Data".chars() {
        app.handle(Key::Char(c));
    }
    let frame = app.snapshot(100, 30).unwrap();
    assert!(
        frame.contains("Data Protection"),
        "the popup offers what is hidden: {frame}"
    );
    app.handle(Key::Enter);
    assert!(
        app.snapshot(100, 30).unwrap().contains("Data Protection"),
        "and `enter` on the row runs `:show`, not `:hide`"
    );
    assert_eq!(
        *saved.lock().unwrap(),
        Some(Vec::new()),
        "the lift is persisted, not only applied"
    );

    // The other verb, over the menu's own names: `Hardware` is a group nothing has hidden.
    app.handle(Key::Char(':'));
    for c in "hide Hardwa".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert!(
        app.sidebar.is_hidden(group_index("Hardware"), 0),
        "`:hide Hardwa` completed to the group and hid it"
    );
}
