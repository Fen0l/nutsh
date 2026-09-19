//! Several Prism Centrals in one table: `:ctx lab dr` joins `dr` beside `lab`, every top-level
//! table lists both with a CONTEXT column, a row acts against its own Prism Central, and a
//! credential refused by one of them ends that one alone.

mod common;

use futures::future::BoxFuture;
use nutsh_core::contexts::{ConnectRequest, Connected, ContextRow, Contexts};
use nutsh_core::session::Scope;
use nutsh_mockpc::MockPc;
use nutsh_prism::Profile;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key};

/// Two named contexts, each a mock, connected with the stored password.
struct TwoPcs {
    rows: Vec<(String, Profile)>,
}

impl Contexts for TwoPcs {
    fn list(&self) -> anyhow::Result<Vec<ContextRow>> {
        Ok(self
            .rows
            .iter()
            .enumerate()
            .map(|(i, (name, p))| ContextRow {
                name: name.clone(),
                host: p.host.clone(),
                port: p.port,
                username: p.username.clone(),
                cluster: None,
                readonly: false,
                insecure: false,
                current: i == 0,
                has_password: true,
            })
            .collect())
    }
    fn connect(&self, req: ConnectRequest) -> BoxFuture<'static, anyhow::Result<Connected>> {
        let ConnectRequest::Stored { name } = req else {
            return Box::pin(async { anyhow::bail!("this test connects stored contexts only") });
        };
        let profile = self
            .rows
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| p.clone());
        Box::pin(async move {
            let profile = profile.ok_or_else(|| anyhow::anyhow!("no context named {name}"))?;
            let session = nutsh_core::session::connect(
                &profile,
                "secret",
                Scope {
                    context: Some(name),
                    ..Scope::default()
                },
                None,
            )
            .await?;
            Ok(Connected {
                session,
                warnings: Vec::new(),
                cache_dir: None,
                restored: None,
            })
        })
    }
    fn remove(&self, _name: &str) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("not in this test")
    }
}

/// An app on `lab`'s VM table, with `dr` one `:ctx lab dr` away.
async fn lab_with_dr(lab: &MockPc, dr: &MockPc) -> App {
    let session = nutsh_core::session::connect(
        &common::profile(lab),
        "secret",
        Scope {
            context: Some("lab".into()),
            ..Scope::default()
        },
        None,
    )
    .await
    .unwrap();
    let contexts = TwoPcs {
        rows: vec![
            ("lab".to_string(), common::profile(lab)),
            ("dr".to_string(), common::profile(dr)),
        ],
    };
    let mut app = App::open(session, Box::new(contexts), "vm", common::config()).unwrap();
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    common::settle_stats(&mut app).await;
    app
}

fn command(app: &mut App, line: &str) {
    app.handle(Key::Char(':'));
    for c in line.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

async fn join_dr(app: &mut App) {
    command(app, "ctx lab dr");
    tokio::time::timeout(std::time::Duration::from_secs(10), app.await_connect())
        .await
        .expect("dr connects");
    assert_eq!(app.peers_for_test(), ["dr"]);
    tokio::time::timeout(std::time::Duration::from_secs(10), app.settle_peers_once())
        .await
        .expect("dr's VM list lands");
}

fn presentations(pc: &MockPc) -> usize {
    pc.requests().iter().filter(|r| r.authenticates()).count()
}

/// `:ctx lab dr` keeps `lab` as the session and lists `dr` beside it: one table, a CONTEXT
/// column, both counted, the header naming both. `:ctx lab` alone drops it again.
#[tokio::test]
async fn ctx_with_two_names_lists_both_prism_centrals_in_one_table() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;
    assert!(!app.snapshot(120, 24).unwrap().contains("CONTEXT"));

    join_dr(&mut app).await;
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains("Virtual Machines [6]"), "{frame}");
    assert!(frame.contains("CONTEXT"), "{frame}");
    assert!(
        frame
            .lines()
            .any(|l| l.contains("web-01") && l.contains("lab"))
            && frame
                .lines()
                .any(|l| l.contains("web-01") && l.contains("dr")),
        "each row says where it is from: {frame}"
    );
    assert!(frame.contains("Context:    lab +dr"), "{frame}");
    let settings = common::settings(&lab);
    settings.bind(|| insta::assert_snapshot!("two_prism_centrals", frame.clone()));

    // `/dr` keeps one Prism Central's rows, the way `/` narrows on a name.
    app.handle(Key::Char('/'));
    for c in "dr".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains("Virtual Machines [3 of 6] /dr"), "{frame}");
    app.handle(Key::Char('/'));
    app.handle(Key::Esc);

    command(&mut app, "ctx lab");
    assert!(app.peers_for_test().is_empty());
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains("Virtual Machines [3]"), "{frame}");
    assert!(!frame.contains("CONTEXT"), "{frame}");
}

/// The rule the whole feature rests on: a 401 on one Prism Central is terminal for that one
/// and only that one. `dr` keeps the rows it had and stops asking; `lab` never notices.
#[tokio::test]
async fn a_refused_credential_on_one_peer_ends_that_peer_alone() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;
    join_dr(&mut app).await;
    let lab_presented = presentations(&lab);
    let dr_presented = presentations(&dr);

    // The password stops opening a session on dr; the next request to renew learns it.
    dr.expire_session();
    dr.refuse_credential();
    app.handle(Key::Ctrl('r'));
    common::settle(&mut app).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), app.settle_peers_once())
        .await
        .expect("dr's cycle ends");

    let frame = app.snapshot(120, 24).unwrap();
    assert!(
        frame.contains("Virtual Machines [6]"),
        "dr's rows stay drawn: {frame}"
    );
    assert_eq!(
        presentations(&dr),
        dr_presented + 1,
        "one presentation on renewal, refused, and never another"
    );
    assert_eq!(
        presentations(&lab),
        lab_presented,
        "lab was never asked again"
    );

    // Another refresh: lab answers, dr is not asked at all.
    let dr_requests = dr.requests().len();
    let lab_requests = lab.requests().len();
    app.handle(Key::Ctrl('r'));
    common::settle(&mut app).await;
    assert!(lab.requests().len() > lab_requests, "lab keeps polling");
    assert_eq!(dr.requests().len(), dr_requests, "dr is latched");
    assert_eq!(app.peers_for_test(), ["dr"], "still joined, just stopped");
}

/// An action on a row that came from `dr` goes to `dr`, the confirm says so, and the journal
/// records the context it ran in.
#[tokio::test]
async fn an_action_on_a_peer_row_goes_to_that_prism_central() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;
    join_dr(&mut app).await;
    // The primary's three rows come first; the fourth is dr's web-01.
    for _ in 0..3 {
        app.handle(Key::Char('j'));
    }
    let frame = app.snapshot(120, 24).unwrap();
    let row = frame
        .lines()
        .find(|l| l.contains("▌ web-01"))
        .unwrap_or_else(|| panic!("{frame}"));
    assert!(row.contains("dr"), "the cursor is on dr's row: {row}");

    app.handle(Key::Char('P'));
    assert_eq!(app.mode, Mode::Confirm);
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains("web-01 on dr? [y/N]"), "{frame}");
    app.handle(Key::Char('y'));
    let lab_tasks = lab.requests_to("/config/tasks").len();
    common::settle_acted(&mut app).await;
    assert_eq!(dr.requests_to("/$actions/power-off").len(), 1, "dr got it");
    assert_eq!(
        lab.requests_to("/$actions/power-off").len(),
        0,
        "lab did not"
    );
    // The task the action returned is dr's: the watch reads it there, and nothing of lab's
    // is asked or marked failed for it.
    tokio::time::timeout(std::time::Duration::from_secs(10), app.settle_entity_once())
        .await
        .expect("the watch reads the task");
    assert!(
        dr.requests()
            .iter()
            .any(|r| r.method == "GET" && r.path.contains("/config/tasks/")),
        "dr is polled for its task"
    );
    assert_eq!(
        lab.requests_to("/config/tasks").len(),
        lab_tasks,
        "lab is not asked about dr's task"
    );
    let live = app.live.as_ref().unwrap();
    for (key, table) in live.store.tables() {
        assert!(
            table.error.is_none(),
            "{} on {:?} carries an error: {:?}",
            key.kind.id,
            key.context,
            table.error
        );
    }

    command(&mut app, "journal");
    let frame = app.snapshot(120, 24).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("power-off") && l.contains("dr")),
        "the journal names the context: {frame}"
    );
}

/// The Contexts screen joins and drops with `space`: the row reads `+` while joined, the
/// session's own row refuses, and the table behind it merges and unmerges accordingly.
#[tokio::test]
async fn space_on_the_contexts_screen_joins_and_drops_a_peer() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;

    command(&mut app, "ctx");
    assert_eq!(app.mode, Mode::Contexts);
    app.handle(Key::Char(' '));
    assert_eq!(
        app.screen.message.as_deref(),
        Some("lab is the session"),
        "the session's own row cannot be a peer"
    );
    app.handle(Key::Char('j'));
    app.handle(Key::Char(' '));
    tokio::time::timeout(std::time::Duration::from_secs(10), app.await_connect())
        .await
        .expect("dr connects");
    assert_eq!(app.peers_for_test(), ["dr"]);
    let frame = app.snapshot(120, 20).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains(" dr ") && l.contains("[x]")),
        "the joined row is ticked: {frame}"
    );
    assert!(
        frame
            .lines()
            .any(|l| l.contains(" lab ") && l.contains("session")),
        "the session's row says so: {frame}"
    );

    app.handle(Key::Esc);
    tokio::time::timeout(std::time::Duration::from_secs(10), app.settle_peers_once())
        .await
        .expect("dr's rows land");
    assert!(
        app.snapshot(120, 24)
            .unwrap()
            .contains("Virtual Machines [6]")
    );

    command(&mut app, "ctx");
    app.handle(Key::Char('j'));
    app.handle(Key::Char(' '));
    assert!(app.peers_for_test().is_empty(), "space again drops it");
    assert_eq!(app.screen.message.as_deref(), Some("dr dropped"));
    app.handle(Key::Esc);
    assert!(
        app.snapshot(120, 24)
            .unwrap()
            .contains("Virtual Machines [3]")
    );
}

/// A name that is not a context, or a name given twice: `-c lab,typo` reports and goes on,
/// `:ctx lab dr dr` joins dr once and presents its credential once.
#[tokio::test]
async fn a_peer_that_cannot_be_joined_says_so_without_waiting() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.join(&["typo".to_string()]),
    )
    .await
    .expect("nothing to wait for");
    assert_eq!(app.status.as_deref(), Some("no context named typo"));
    assert!(app.peers_for_test().is_empty());

    command(&mut app, "ctx lab dr dr");
    tokio::time::timeout(std::time::Duration::from_secs(10), app.await_connect())
        .await
        .expect("dr connects");
    tokio::time::timeout(std::time::Duration::from_secs(10), app.settle_peers_once())
        .await
        .expect("dr's rows land");
    assert_eq!(app.peers_for_test(), ["dr"]);
    assert_eq!(presentations(&dr), 1, "one credential on the wire");
    assert!(
        app.snapshot(120, 24)
            .unwrap()
            .contains("Virtual Machines [6]")
    );
}

/// A peer dropped and joined again lists its rows again: the view forgot the subscription
/// that died with it.
#[tokio::test]
async fn a_dropped_peer_joins_again_with_its_rows() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;
    join_dr(&mut app).await;
    command(&mut app, "ctx lab");
    assert!(app.peers_for_test().is_empty());
    assert!(
        app.snapshot(120, 24)
            .unwrap()
            .contains("Virtual Machines [3]")
    );
    join_dr(&mut app).await;
    assert!(
        app.snapshot(120, 24)
            .unwrap()
            .contains("Virtual Machines [6]")
    );
}

/// A mark on a peer's row is drawn on that row and no other, though lab holds a VM with the
/// same extId.
#[tokio::test]
async fn a_mark_on_a_peer_row_is_drawn_on_that_row() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;
    join_dr(&mut app).await;
    for _ in 0..3 {
        app.handle(Key::Char('j'));
    }
    app.handle(Key::Char(' '));
    let frame = app.snapshot(120, 24).unwrap();
    let marked: Vec<&str> = frame.lines().filter(|l| l.contains("* web-01")).collect();
    assert_eq!(marked.len(), 1, "one row is marked: {frame}");
    assert!(marked[0].contains(" dr "), "dr's row: {}", marked[0]);
}

/// `⏎` on a peer's row lists its children from that Prism Central, under a view that says
/// so, and `esc` comes back to the merged table.
#[tokio::test]
async fn drilling_into_a_peer_row_lists_its_children_from_that_prism_central() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let mut app = lab_with_dr(&lab, &dr).await;
    join_dr(&mut app).await;
    for _ in 0..3 {
        app.handle(Key::Char('j'));
    }
    let lab_disks = lab.requests_to("/disks").len();
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Picker);
    for c in "disk".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    {
        let view = app.view().unwrap();
        assert_eq!(view.key.kind.id, "vmm.ahv.config.Disk");
        assert_eq!(view.key.context.as_deref(), Some("dr"));
        assert_eq!(
            view.key.parents,
            vec!["3d0c4a2e-1b8f-4c1a-9e2f-000000000001".to_string()]
        );
    }
    common::settle(&mut app).await;
    assert!(!dr.requests_to("/disks").is_empty(), "dr lists the disks");
    assert_eq!(
        lab.requests_to("/disks").len(),
        lab_disks,
        "lab is not asked for dr's disks"
    );
    let frame = app.snapshot(120, 24).unwrap();
    assert!(frame.contains("› web-01 › Disks"), "{frame}");
    app.handle(Key::Esc);
    assert!(
        app.snapshot(120, 24)
            .unwrap()
            .contains("Virtual Machines [6]"),
        "back on the merged table"
    );
}
