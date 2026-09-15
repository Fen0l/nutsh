mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key};

async fn vms(pc: &MockPc) -> App {
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

async fn tasks(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "tasks",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app
}

/// `j` until the cursor is on `name`, bounded by the rows there are: a table that never shows
/// the row would otherwise hang the test instead of failing it.
fn select(app: &mut App, name: &str) {
    for _ in 0..64 {
        if app.selected_name() == Some(name) {
            return;
        }
        app.handle(Key::Char('j'));
    }
    panic!("no row named {name}");
}

#[tokio::test]
async fn the_footer_carries_the_running_task_and_coexists_with_a_status() {
    let pc = MockPc::builder().stall_task("power-off").start().await;
    let mut app = vms(&pc).await;
    app.handle(Key::Char('P'));
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    app.tick(common::now(), std::time::Instant::now());
    let frame = app.snapshot(120, 12).unwrap();
    assert!(frame.contains("power-off web-01 queued 0%"), "{frame}");
    // The two live on different rows, so neither hides the other: `q` with a task running asks
    // instead of quitting, and the question and the watch are on the same frame.
    app.handle(Key::Char('q'));
    assert!(
        !app.should_quit(),
        "a running task turns the first q into a question"
    );
    let frame = app.snapshot(120, 12).unwrap();
    assert!(
        frame.contains("1 task still running; press q again to quit"),
        "{frame}"
    );
    assert!(
        frame.contains("power-off web-01 queued 0%"),
        "the watch still holds its own line: {frame}"
    );
}

#[tokio::test]
async fn the_tasks_table_refuses_cancel_on_a_task_that_is_not_cancelable() {
    let pc = MockPc::builder().start().await;
    let mut app = tasks(&pc).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("tasks_table", app.snapshot(120, 18).unwrap()));
    // The first fixture task is SUCCEEDED and not cancelable.
    app.handle(Key::Char('c'));
    assert_eq!(app.status.as_deref(), Some("not cancelable"));
    assert_eq!(app.mode, Mode::Table);
    // The refusal is composed in `core::actions::reason`, so the menu greys the row with the
    // very same words rather than letting `enter` find out.
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu);
    let frame = app.snapshot(120, 18).unwrap();
    assert!(frame.contains("Cancel task"), "{frame}");
    assert!(frame.contains("not cancelable"), "{frame}");
    app.handle(Key::Esc);
    // And `:can-i` asks the same function, so all three say the same words about the same row.
    app.handle(Key::Char(':'));
    for c in "can-i cancel tasks".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert_eq!(
        app.status.as_deref(),
        Some("cancel on Tasks: not cancelable")
    );
    // The RUNNING one is.
    select(&mut app, "VmMigrate");
    app.handle(Key::Char('c'));
    assert_eq!(app.mode, Mode::Confirm);
}

#[tokio::test]
async fn the_journal_lists_what_was_attempted_including_what_was_refused() {
    let pc = MockPc::builder().start().await;
    let session = common::session(&pc).await;
    let config = common::config_with_guardrails(
        "[[guardrails]]\nactions = [\"power-on\"]\ndeny = true\nreason = \"nope\"\n",
    );
    let mut app = App::open(session, Box::new(common::NoContexts), "vm", config).unwrap();
    common::settle(&mut app).await;
    // The header's two pollers are settled here, before anything is sent, so the counters the
    // snapshot records are the fixture's rather than a race between the stats cycle's seven
    // round trips and this test's own POST.
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    // An empty ring says so, and this is the only frame that can show it: the next key acts.
    app.handle(Key::Char(':'));
    for c in "journal".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Journal);
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("Nothing attempted this session."), "{frame}");
    app.handle(Key::Esc);
    app.handle(Key::Char('P'));
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    app.handle(Key::Char('p')); // denied
    app.handle(Key::Char(':'));
    for c in "journal".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Journal);
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("journal", app.snapshot(120, 14).unwrap()));
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("power-off"), "{frame}");
    assert!(
        frame.contains("power-on"),
        "a denial is journalled too: {frame}"
    );
    assert!(frame.contains("denied: nope"), "{frame}");
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
}

#[tokio::test]
async fn can_i_takes_two_arguments_and_prints_what_reason_returns() {
    let pc = MockPc::builder().start().await;
    let mut app = vms(&pc).await;
    let ask = |app: &mut App, text: &str| {
        app.handle(Key::Char(':'));
        for c in text.chars() {
            app.handle(Key::Char(c));
        }
        app.handle(Key::Enter);
        app.status.clone().unwrap_or_default()
    };
    let line = ask(&mut app, "can-i power-off vm");
    assert!(
        line.starts_with("power-off on Virtual Machines: "),
        "{line}"
    );
    // The mock serves no `iam`, so can-i is honestly unknown and nothing is blocked.
    assert!(line.contains("unknown"), "{line}");
    assert_eq!(ask(&mut app, "can-i power-off vmz"), "no kind matching vmz");
    assert_eq!(
        ask(&mut app, "can-i nope vm"),
        "no action nope on Virtual Machines"
    );
    assert_eq!(
        ask(&mut app, "can-i vm"),
        ":can-i takes an action and a kind"
    );
}

#[tokio::test]
async fn an_action_that_answers_with_a_document_opens_the_detail_pane_over_it() {
    let pc = MockPc::builder().start().await;
    let mut app = tasks(&pc).await;
    select(&mut app, "VmMigrate");
    app.handle(Key::Char('c'));
    app.handle(Key::Char('y'));
    // The document arrives on the poll channel, so it can land on any frame - here on a palette
    // being typed into. It takes the screen, and it takes every modal with it: what `esc` goes
    // back to is the table, so nothing may be left open behind the pane.
    app.handle(Key::Char(':'));
    app.handle(Key::Char('v'));
    common::settle_acted(&mut app).await;
    assert_eq!(app.mode, Mode::Detail);
    assert!(app.palette.is_none(), "the palette went with it");
    let frame = app.snapshot(100, 14).unwrap();
    assert!(frame.contains("cancellation requested"), "{frame}");
    app.handle(Key::Esc);
    assert_eq!(
        app.mode,
        Mode::Table,
        "a payload pane has no subscription to drop"
    );
}

/// `y` on a failed task the fixture holds: under the task's own sections - which already name
/// the entity it touched - the alerts raised in the ten minutes either side of its start (the
/// one at 09:50:46 is in, the one at 09:40:00 is a second outside) and a journal that says this
/// session did not start it.
#[tokio::test]
async fn a_failed_task_pane_gathers_the_entity_the_alerts_and_the_journal() {
    let pc = MockPc::builder().start().await;
    let mut app = tasks(&pc).await;
    common::settle_names(&mut app).await;
    common::settle_stats(&mut app).await;
    select(&mut app, "VmDelete");
    app.handle(Key::Char('y'));
    common::settle(&mut app).await;
    app.settle_once_list().await;
    let frame = app.snapshot(120, 40).unwrap();
    assert!(
        frame.contains("db-01"),
        "the entity the task touched: {frame}"
    );
    assert!(frame.contains("ALERTS 09:40–10:00 UTC"), "{frame}");
    assert!(
        frame.contains("Disk space usage high"),
        "inside the window: {frame}"
    );
    assert!(
        !frame.contains("is unreachable"),
        "a second outside the window is outside: {frame}"
    );
    assert!(frame.contains("not started from here"), "{frame}");
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("failed_task_evidence", frame));
}

/// A task this session started and the mock failed: the journal section carries the action,
/// the outcome and when, off the entry the journal already holds for it.
#[tokio::test]
async fn a_failed_task_this_session_started_shows_its_journal_entry() {
    let pc = MockPc::builder().fail_task("power-off").start().await;
    let mut app = vms(&pc).await;
    app.handle(Key::Char('P'));
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    // The watch follows the task to its end, which is when the journal learns it failed; the
    // Tasks table shows the same end, so it is what the test waits on.
    app.open_root(nutsh_catalog::kind("prism.config.Task").unwrap());
    for _ in 0..12 {
        common::settle(&mut app).await;
        let frame = app.snapshot(120, 40).unwrap();
        if frame
            .lines()
            .any(|l| l.contains("power-off on web-01") && l.contains("FAILED"))
        {
            break;
        }
    }
    select(&mut app, "power-off");
    app.handle(Key::Char('y'));
    common::settle(&mut app).await;
    let frame = app.snapshot(120, 40).unwrap();
    assert!(frame.contains("JOURNAL"), "{frame}");
    assert!(frame.contains("power-off on web-01"), "{frame}");
    assert!(frame.contains("failed"), "{frame}");
    assert!(!frame.contains("not started from here"), "{frame}");
}
