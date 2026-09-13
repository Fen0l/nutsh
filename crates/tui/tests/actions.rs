mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::{Config, Mode};
use nutsh_tui::{App, Key};

async fn app(pc: &MockPc) -> App {
    app_with(pc, common::config()).await
}

/// `app` for a test that needs guardrails of its own.
async fn app_with(pc: &MockPc, config: Config) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(session, Box::new(common::NoContexts), "vm", config).unwrap();
    common::settle(&mut app).await;
    app
}

/// The header's counters and the names behind the `CLUSTER` and `HOST` cells are part of every
/// frame, so a snapshot settles the two pollers that fill them rather than racing them -
/// exactly as the table snapshots do.
async fn app_for_snapshot(pc: &MockPc) -> App {
    app_for_snapshot_with(pc, common::config()).await
}

/// The same, for a snapshot test that needs its own `Config`. It exists so that needing
/// guardrails is not a reason to build the app inline and thereby skip the settling: that is
/// how `vm_menu_denied` came to be recorded against unresolved names while its sibling
/// `vm_menu` was recorded against resolved ones.
async fn app_for_snapshot_with(pc: &MockPc, config: Config) -> App {
    let mut app = app_with(pc, config).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app
}

#[tokio::test]
async fn space_marks_and_moves_and_esc_clears_before_popping() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(' '));
    assert_eq!(app.marks(), vec!["3d0c4a2e-1b8f-4c1a-9e2f-000000000001"]);
    assert_eq!(app.selected_name(), Some("web-02"), "and moves down one");
    app.handle(Key::Char(' '));
    assert_eq!(app.marks().len(), 2);
    // The mark is in the frame, not only in the state: it goes in front of the first cell,
    // since the gutter ratatui gives a table is the cursor's.
    let frame = app.snapshot(120, 19).unwrap();
    assert!(frame.contains("* web-01"), "{frame}");
    assert!(frame.contains("* web-02"), "{frame}");
    assert!(!frame.contains("* db-01"), "{frame}");
    app.handle(Key::Esc);
    assert!(app.marks().is_empty(), "esc clears marks first");
    assert_eq!(app.mode, Mode::Table, "and never surprises by popping");
}

#[tokio::test]
async fn the_menu_lists_the_curated_actions_and_greys_what_cannot_run() {
    let pc = MockPc::builder().start().await;
    let mut app = app_for_snapshot(&pc).await;
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu);
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("vm_menu", app.snapshot(120, 30).unwrap()));
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("Power off"), "{frame}");
    assert!(frame.contains("Create recovery point"), "{frame}");
}

/// The complaint that started this: "action box for VM is smaller than before we dont see all
/// action". A VM has twenty-five of them, every one reachable with `Down`, and nothing drawn
/// on the frame admitted the rest were there. Now the box says so at every size it is drawn at.
#[tokio::test]
async fn the_action_menu_says_what_is_off_the_end_of_it() {
    let pc = MockPc::builder().start().await;
    let mut app = app_for_snapshot(&pc).await;
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu);
    assert_eq!(app.menu_actions().len(), 25, "a VM has twenty-five actions");
    // Room for all of them, so the box says nothing: no count, no arrows.
    let frame = app.snapshot(120, 40).unwrap();
    assert!(
        frame.contains("act on web-01 ─"),
        "a list that fits carries no count: {frame}"
    );
    // The modal is allowed the header's rows, and those seven are what turn eighteen of the
    // twenty-five into all of them.
    let frame = app.snapshot(100, 30).unwrap();
    assert!(
        frame.contains("transfer-files-to-guest"),
        "the last actions are on screen at 100x30: {frame}"
    );
    assert!(
        frame.contains("act on web-01 ─"),
        "and still no count: {frame}"
    );
    // Twenty-four rows hold no arrangement of twenty-five actions, so the box counts what it
    // has and points at what it has not.
    let frame = app.snapshot(80, 24).unwrap();
    assert!(frame.contains("· 19/25"), "the title counts: {frame}");
    assert!(
        frame.contains("↓6"),
        "and the bottom border points down: {frame}"
    );
    assert!(
        !frame.contains("↑6"),
        "nothing above the first row: {frame}"
    );
    // Scrolled to the end, the arrow turns around.
    for _ in 0..25 {
        app.handle(Key::Down);
    }
    let frame = app.snapshot(80, 24).unwrap();
    assert!(frame.contains("↑6"), "{frame}");
    assert!(!frame.contains("↓6"), "{frame}");
}

/// The `hidden` filter, asserted on the one kind in the catalog that has a hidden action:
/// `manage-alert` is the raw API action the curated `acknowledge` and `resolve` are made of,
/// and listing it too would offer the same two things twice under a name nobody curated.
#[tokio::test]
async fn a_hidden_action_is_not_in_the_menu() {
    let pc = MockPc::builder().start().await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "alert",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu);
    let frame = app.snapshot(120, 20).unwrap();
    assert!(frame.contains("Acknowledge"), "{frame}");
    assert!(frame.contains("Resolve"), "{frame}");
    assert!(
        !frame.contains("manage-alert"),
        "hidden actions stay hidden: {frame}"
    );
    let kind = nutsh_catalog::kind("monitoring.serviceability.Alert").expect("the catalog has it");
    assert!(
        kind.actions.iter().any(|a| a.hidden),
        "the case is only worth anything while the kind has a hidden action"
    );
    assert_eq!(
        app.menu_actions(),
        kind.actions
            .iter()
            .filter(|a| !a.hidden)
            .map(|a| a.name)
            .collect::<Vec<_>>(),
        "every action of the kind but the hidden ones"
    );
}

#[tokio::test]
async fn a_denied_action_is_listed_and_greyed_with_its_reason() {
    let pc = MockPc::builder().start().await;
    let config = common::config_with_guardrails(
        r#"
[[guardrails]]
actions = ["power-off"]
deny    = true
reason  = "Prod changes go through change control."
"#,
    );
    let mut app = app_for_snapshot_with(&pc, config).await;
    app.handle(Key::Char('a'));
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("vm_menu_denied", app.snapshot(120, 30).unwrap()));
    // `enter` on a greyed row puts the reason in the status line and runs nothing.
    while app.menu_selected_action() != Some("power-off") {
        app.handle(Key::Down);
    }
    app.handle(Key::Enter);
    assert_eq!(
        app.status.as_deref(),
        Some("denied: Prod changes go through change control.")
    );
    assert_eq!(app.mode, Mode::Menu);
    assert!(pc.requests_to("/$actions/power-off").is_empty());
}

#[tokio::test]
async fn a_curated_key_runs_the_same_plan_as_the_menu() {
    let pc = MockPc::builder().start().await;
    let mut app = app_for_snapshot(&pc).await;
    app.handle(Key::Char('P'));
    assert_eq!(app.mode, Mode::Confirm, "medium danger asks");
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("confirm_yes", app.snapshot(120, 20).unwrap()));
    app.handle(Key::Char('n'));
    assert_eq!(app.mode, Mode::Table, "every other key cancels");
    assert!(pc.requests_to("/$actions/power-off").is_empty());

    app.handle(Key::Char('P'));
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    assert_eq!(pc.requests_to("/$actions/power-off").len(), 1);
    assert!(
        app.status.as_deref().unwrap().contains("task started"),
        "{:?}",
        app.status
    );

    // The same action off the menu, on the same row: the key is a shortcut through the menu's
    // own path, so the two have to send the same request.
    app.handle(Key::Char('a'));
    while app.menu_selected_action() != Some("power-off") {
        app.handle(Key::Down);
    }
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Confirm, "the menu asks what the key asks");
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    let sent = pc.requests_to("/$actions/power-off");
    assert_eq!(sent.len(), 2, "one from the key, one from the menu");
    assert_eq!(sent[0].method, sent[1].method);
    assert_eq!(sent[0].path, sent[1].path);
    assert_eq!(sent[0].body, sent[1].body);
}

#[tokio::test]
async fn a_type_name_confirm_wants_the_name_and_a_mismatch_sends_nothing() {
    let pc = MockPc::builder().start().await;
    let mut app = app_for_snapshot(&pc).await;
    app.handle(Key::Ctrl('d'));
    assert_eq!(app.mode, Mode::Confirm);
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("confirm_type_name", app.snapshot(120, 20).unwrap()));
    for c in "web-0X".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert_eq!(app.status.as_deref(), Some("names do not match"));
    assert!(pc.requests().iter().all(|r| r.method != "DELETE"));
}

/// A `TypeName` confirm over marks cannot ask for seven names, so it asks for one deliberate
/// phrase instead. The built-in cap is five, so the rule that allows seven is part of the case.
#[tokio::test]
async fn a_bulk_type_name_confirm_asks_for_a_phrase() {
    let pc = MockPc::builder().start().await;
    let config = common::config_with_guardrails("[[guardrails]]\nmax_bulk = 10\n");
    let mut app = app_with(&pc, config).await;
    app.inject_rows(7);
    for _ in 0..7 {
        app.handle(Key::Char(' '));
    }
    assert_eq!(app.marks().len(), 7);
    app.handle(Key::Ctrl('d'));
    assert_eq!(app.mode, Mode::Confirm);
    let frame = app.snapshot(120, 20).unwrap();
    assert!(
        frame.contains("Delete 7 Virtual Machines. Type DELETE 7 to confirm:"),
        "{frame}"
    );
    assert!(frame.contains("vm-000"), "{frame}");
    assert!(frame.contains("… and 2 more"), "{frame}");
    // The phrase sends it, and the marks it was gathered from are spent: a second `ctrl-d`
    // must not silently re-run the same seven rows.
    for c in "DELETE 7".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(app.status.as_deref(), Some("7 actions started"));
    assert!(app.marks().is_empty(), "the marks went with the action");
    // And the cap still bites without the rule.
    let session = common::session(&pc).await;
    let mut plain = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut plain).await;
    plain.inject_rows(7);
    for _ in 0..7 {
        plain.handle(Key::Char(' '));
    }
    plain.handle(Key::Ctrl('d'));
    assert_eq!(plain.mode, Mode::Table);
    assert_eq!(
        plain.status.as_deref(),
        Some("7 marked, this guardrail allows 5 at a time")
    );
}

/// An empty table has no subject. Both paths say so rather than doing nothing in silence: the
/// key sets the status line, and the menu titles itself with the same words.
#[tokio::test]
async fn an_empty_table_has_no_subject() {
    let pc = MockPc::builder()
        .missing_path("/vmm/v4.3/ahv/config/vms")
        .start()
        .await;
    let mut app = app(&pc).await;
    assert_eq!(app.selected_name(), None, "nothing to act on");
    app.handle(Key::Char('P'));
    assert_eq!(app.status.as_deref(), Some("no row selected"));
    assert_eq!(app.mode, Mode::Table);
    assert!(pc.requests_to("/$actions/power-off").is_empty());
    app.handle(Key::Char('a'));
    let frame = app.snapshot(120, 20).unwrap();
    assert!(frame.contains("act on no row selected"), "{frame}");
}

#[tokio::test]
async fn a_read_only_session_greys_every_row() {
    let pc = MockPc::builder().start().await;
    let mut session = common::session(&pc).await;
    session.readonly = true;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char('a'));
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("read-only session"), "{frame}");
    app.handle(Key::Esc);
    app.handle(Key::Char('P'));
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(app.status.as_deref(), Some("read-only session"));
}

#[tokio::test]
async fn q_quits_from_a_table_but_asks_once_while_a_task_runs() {
    let pc = MockPc::builder().stall_task("power-off").start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('P'));
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    app.handle(Key::Char('q'));
    assert!(!app.should_quit());
    assert_eq!(
        app.status.as_deref(),
        Some("1 task still running; press q again to quit")
    );
    // Anything in between is a session that went on doing something else, and the warning the
    // second `q` would be answering is stale.
    app.handle(Key::Char('j'));
    app.handle(Key::Char('q'));
    assert!(!app.should_quit(), "the two q's have to be consecutive");
    assert_eq!(
        app.status.as_deref(),
        Some("1 task still running; press q again to quit")
    );
    app.handle(Key::Char('q'));
    assert!(app.should_quit());
}

/// `q` means the same on a page as on a table: a quit that depends on which view is open is one
/// nobody trusts. A page acts on nothing, so the task it asks about is one a table started.
#[tokio::test]
async fn q_asks_the_same_question_from_a_page() {
    let pc = MockPc::builder().stall_task("power-off").start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('P'));
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    let def = nutsh_catalog::page("disaster-recovery").expect("the catalog has the DR page");
    app.open_page(def);
    assert!(app.page().is_some(), "the page is the open view");
    app.handle(Key::Char('q'));
    assert!(!app.should_quit());
    assert_eq!(
        app.status.as_deref(),
        Some("1 task still running; press q again to quit")
    );
    app.handle(Key::Char('q'));
    assert!(app.should_quit());
}

/// The `a` menu, the `⏎` picker and the detail pane draw one list, built once: three lists
/// that can drift apart is worse than one list in one place.
#[tokio::test]
async fn the_three_action_surfaces_list_the_same_actions() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('a'));
    // The menu is the only surface that also offers the collection, so it is the row-scoped
    // part of its list the other two have to match.
    let menu: Vec<&str> = app
        .menu_actions()
        .into_iter()
        .filter(|name| {
            nutsh_catalog::kind("vmm.ahv.config.Vm")
                .and_then(|k| k.action_target().action(name))
                .is_some_and(nutsh_catalog::Action::acts_on_a_row)
        })
        .collect();
    assert!(!menu.is_empty(), "a VM has actions");
    app.handle(Key::Esc);

    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Picker);
    assert_eq!(app.picker_actions(), menu, "the picker's ACTIONS group");
    app.handle(Key::Esc);

    app.handle(Key::Char('y'));
    assert_eq!(app.mode, Mode::Detail);
    assert_eq!(app.detail_action_titles().len(), menu.len());
}

/// `create` makes a *new* VM and does nothing to `web-01`, so the two surfaces that say they
/// are showing what can be done to this row do not list it. It stays on the `a` menu, which is
/// the only place creating a thing is reachable at all.
#[tokio::test]
async fn a_collection_level_action_is_not_offered_on_the_entity_surfaces() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let kind = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
    let create = kind.action("create").expect("VMs are created");
    assert!(!create.acts_on_a_row());
    // And the curated workflow that POSTs to another kind's collection naming this row is not
    // caught by the same rule: it is an action on this VM however its URL reads.
    let snapshot = kind.action("snapshot").expect("VMs snapshot");
    assert!(!snapshot.names_entity() && snapshot.acts_on_a_row());

    app.handle(Key::Enter);
    assert!(!app.picker_actions().contains(&"create"));
    assert!(app.picker_actions().contains(&"snapshot"));
    app.handle(Key::Esc);
    app.handle(Key::Char('y'));
    let frame = app.snapshot(100, 100).unwrap();
    assert!(!frame.contains("ACTIONS ───\n"), "{frame}");
    assert!(
        frame.contains("Create recovery point"),
        "the row's own workflow stays: {frame}"
    );
    assert!(
        !frame.lines().any(|l| l.trim_end().ends_with(" create")),
        "{frame}"
    );
}

/// `⏎` on a row lists what can be opened under it *and* what can be done to it, each action
/// with the key that runs it.
#[tokio::test]
async fn the_picker_lists_actions_under_the_related_kinds() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Picker);
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("vm_picker", app.snapshot(120, 40).unwrap()));
    let frame = app.snapshot(120, 40).unwrap();
    assert!(frame.contains("related"), "{frame}");
    assert!(frame.contains("actions"), "{frame}");
    assert!(frame.contains("Disks"), "{frame}");
    assert!(frame.contains("Power on"), "{frame}");
    assert!(frame.contains("[p]"), "the key is on the row: {frame}");
    // `enter` on an action runs it rather than opening a table.
    type_str(&mut app, "power on");
    app.handle(Key::Enter);
    assert_eq!(
        app.mode,
        Mode::Table,
        "a low-danger action needs no confirm"
    );
    common::settle_acted(&mut app).await;
    assert_eq!(pc.requests_to("/$actions/power-on").len(), 1);
}

/// The detail pane lists the same actions under an ACTIONS heading, and `a` opens the menu
/// from inside it - on the row the pane is over - without losing the pane.
#[tokio::test]
async fn the_detail_body_lists_the_actions_and_a_opens_the_menu_over_it() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    app.handle(Key::Char('y'));
    assert_eq!(app.mode, Mode::Detail);
    // Under the facts, so a frame tall enough to hold the whole pane is what shows it without
    // a scroll position to guess at.
    let frame = app.snapshot(100, 100).unwrap();
    assert!(frame.contains("ACTIONS ───"), "{frame}");
    assert!(
        frame.contains("p Power on"),
        "the key is the label: {frame}"
    );
    assert!(frame.contains("ctrl-d Delete"), "{frame}");
    // And the last of them is reachable by scrolling: `G` measures the pane off the same lines
    // the section was drawn into, so a section the scroll did not know about could not be read.
    app.handle(Key::Char('G'));
    let frame = app.snapshot(100, 40).unwrap();
    let last = *app.detail_action_titles().last().expect("a VM has actions");
    assert!(frame.contains(last), "{last} is off the end: {frame}");
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu);
    let frame = app.snapshot(100, 100).unwrap();
    assert!(frame.contains("act on web-01"), "{frame}");
    assert!(
        frame.contains("Virtual Machines · web-01"),
        "the pane stays behind the menu: {frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Detail, "esc goes back to the pane");
}

/// A page is a view like a table, and the Dashboard is the launch view: an action that cannot
/// be reached from it is an action most sessions never see.
#[tokio::test]
async fn a_opens_the_menu_on_a_page() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let def = nutsh_catalog::page("disaster-recovery").expect("the catalog has the DR page");
    app.open_page(def);
    common::settle_page(&mut app).await;
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu, "{:?}", app.status);
    assert!(!app.menu_actions().is_empty());
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle(Key::Char(c));
    }
}

/// The curated workflows lead and the generated names follow, with a rule between them on every
/// surface that draws the list. Ordering alone was not enough: a list with no rule through it
/// reads as one list, and `add-custom-attributes` then looks like a command somebody meant.
#[tokio::test]
async fn a_rule_separates_the_curated_workflows_from_the_generated_names() {
    let pc = MockPc::builder().start().await;
    let mut app = app_for_snapshot(&pc).await;

    app.handle(Key::Char('a'));
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("vm_menu_grouped", app.snapshot(120, 40).unwrap()));
    let frame = app.snapshot(120, 40).unwrap();
    let rule = rule_line(&frame).expect("the menu draws the rule");
    assert!(
        rule.contains("raw API names"),
        "the rule says what is under it: {frame}"
    );
    // Above it the labels somebody wrote, below it the names the generator lifted.
    let lines: Vec<&str> = frame.lines().collect();
    let at = lines.iter().position(|l| *l == rule).expect("found above");
    assert!(
        lines[..at].iter().any(|l| l.contains("Power on")),
        "{frame}"
    );
    assert!(
        lines[at..]
            .iter()
            .any(|l| l.contains("add-custom-attributes")),
        "{frame}"
    );
    assert!(
        !lines[..at]
            .iter()
            .any(|l| l.contains("add-custom-attributes")),
        "{frame}"
    );
    app.handle(Key::Esc);

    // The same rule in the same place on the other two surfaces: one list, one boundary. Both
    // are read on a frame tall enough to hold the whole list, since the rule sits below the
    // curated half and neither box is a screenful at ordinary heights.
    app.handle(Key::Enter);
    assert!(
        rule_line(&app.snapshot(120, 100).unwrap()).is_some(),
        "the picker: {}",
        app.snapshot(120, 100).unwrap()
    );
    app.handle(Key::Esc);
    app.handle(Key::Char('y'));
    assert!(
        rule_line(&app.snapshot(100, 100).unwrap()).is_some(),
        "the detail pane: {}",
        app.snapshot(100, 100).unwrap()
    );
}

/// The line the rule is drawn on, whichever surface drew it.
fn rule_line(frame: &str) -> Option<String> {
    frame
        .lines()
        .find(|l| l.contains("raw API names"))
        .map(str::to_string)
}

/// The cursor never stands on the rule, and the selection the frame highlights is the one
/// `enter` would take: the rule is a row of the list, so an index that forgot it would run the
/// two apart.
#[tokio::test]
async fn the_rule_is_not_selectable_and_the_highlight_follows_the_cursor() {
    let pc = MockPc::builder().start().await;
    let mut app = app_for_snapshot(&pc).await;
    app.handle(Key::Char('a'));
    let mut seen = 0;
    for _ in 0..app.menu_actions().len() {
        let chosen = app
            .menu_selected_action()
            .expect("something is always selected");
        let title = nutsh_catalog::kind("vmm.ahv.config.Vm")
            .and_then(|k| k.action_target().action(chosen))
            .expect("the catalog has it")
            .title();
        let frame = app.snapshot(120, 40).unwrap();
        // The highlight marker sits on the row `enter` would run, never on the rule.
        assert!(
            frame.contains(&format!("▌ {title}")),
            "{chosen} is selected but {title:?} is not highlighted: {frame}"
        );
        seen += 1;
        app.handle(Key::Down);
    }
    assert_eq!(seen, app.menu_actions().len());
}

/// `D` and `R` run the graceful pair, which had no key at all and could be reached only through
/// the menu. They sit beside `d` and `r`, which ask the same of the guest through ACPI.
#[tokio::test]
async fn the_guest_keys_run_the_guest_actions() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    for (key, path, form) in [
        ('d', "/$actions/shutdown", false),
        // The guest pair declares a required body whose one property is optional, so the key
        // opens the generated form first and `enter` submits it empty. That is what the menu
        // has always done with them; the key is a shortcut through the same path, not past it.
        ('D', "/$actions/guest-shutdown", true),
        ('r', "/$actions/reboot", false),
        ('R', "/$actions/guest-reboot", true),
    ] {
        app.handle(Key::Char(key));
        if form {
            assert_eq!(app.mode, Mode::Fields, "{key} collects a body");
            app.handle(Key::Enter);
        }
        assert_eq!(app.mode, Mode::Confirm, "{key} asks first");
        app.handle(Key::Char('y'));
        common::settle_acted(&mut app).await;
        assert_eq!(pc.requests_to(path).len(), 1, "{key} sent {path}");
    }
}
