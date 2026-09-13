mod common;

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use nutsh_core::contexts::{ConnectRequest, Connected, ContextRow, Contexts, ENV_ROW};
use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::contexts::{self, Prompt};
use nutsh_tui::{App, Key};

/// Scripted contexts: rows to show, and a connect that succeeds against the mock for
/// `Login`/`Add` with password `secret` and fails otherwise, recording every request.
struct FakeContexts {
    rows: Mutex<Vec<ContextRow>>,
    /// The Prism Central every connect actually reaches, whatever the row says.
    endpoint: Mutex<(String, u16)>,
    /// What `list` fails with, for a config file that cannot be read.
    broken: Mutex<Option<String>>,
    requests: Mutex<Vec<ConnectRequest>>,
    /// Warnings the next successful connect reports.
    warn: Mutex<Vec<String>>,
}

impl FakeContexts {
    fn new(pc: &MockPc, rows: Vec<ContextRow>) -> Arc<FakeContexts> {
        Arc::new(FakeContexts {
            rows: Mutex::new(rows),
            endpoint: Mutex::new((pc.host(), pc.port())),
            broken: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            warn: Mutex::new(Vec::new()),
        })
    }

    /// Point the next connect at another Prism Central, as switching context does.
    fn point_at(&self, pc: &MockPc) {
        *self.endpoint.lock().unwrap() = (pc.host(), pc.port());
    }
}

fn row(name: &str, current: bool, has_password: bool) -> ContextRow {
    ContextRow {
        name: name.into(),
        host: "pc.lab.example".into(),
        port: 9440,
        username: "admin".into(),
        cluster: None,
        readonly: false,
        insecure: true,
        current,
        has_password,
    }
}

/// The seam over a shared `FakeContexts`. Both the trait and `Arc` are foreign to this crate,
/// so the impl needs a local type of its own.
struct Fake(Arc<FakeContexts>);

impl Fake {
    fn boxed(fake: &Arc<FakeContexts>) -> Box<dyn Contexts> {
        Box::new(Fake(fake.clone()))
    }
}

impl Contexts for Fake {
    fn list(&self) -> anyhow::Result<Vec<ContextRow>> {
        if let Some(broken) = self.0.broken.lock().unwrap().as_deref() {
            anyhow::bail!("{broken}");
        }
        Ok(self.0.rows.lock().unwrap().clone())
    }

    fn connect(&self, req: ConnectRequest) -> BoxFuture<'static, anyhow::Result<Connected>> {
        self.0.requests.lock().unwrap().push(req.clone());
        let (host, port) = self.0.endpoint.lock().unwrap().clone();
        let me = self.0.clone();
        Box::pin(async move {
            let (password, name) = match &req {
                ConnectRequest::Stored { name } => {
                    let found = me
                        .rows
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|r| &r.name == name)
                        .cloned();
                    match found {
                        Some(r) if r.has_password => ("secret".to_string(), name.clone()),
                        Some(_) => anyhow::bail!("no stored password for {name}"),
                        None => anyhow::bail!("no context named {name}"),
                    }
                }
                ConnectRequest::Login { name, password } => (password.clone(), name.clone()),
                ConnectRequest::Add(new) => (new.password.clone(), new.name.clone()),
                ConnectRequest::Env { password } => {
                    (password.clone().unwrap_or_default(), ENV_ROW.to_string())
                }
            };
            if password != "secret" {
                anyhow::bail!("password for admin@{host} rejected; nothing stored");
            }
            let profile = nutsh_prism::Profile {
                host,
                port,
                username: "admin".into(),
                verify_tls: true,
                ca_bundle: None,
                plain_http: true,
            };
            let scope = nutsh_core::session::Scope {
                context: Some(name.clone()),
                cluster: None,
                readonly: false,
            };
            let session = nutsh_core::session::connect(&profile, &password, scope, None).await?;
            if let ConnectRequest::Add(new) = &req {
                me.rows.lock().unwrap().push(row(&new.name, false, true));
            }
            if let ConnectRequest::Login { name, .. } = &req {
                for r in me.rows.lock().unwrap().iter_mut() {
                    if &r.name == name {
                        r.has_password = true;
                    }
                }
            }
            let warnings = std::mem::take(&mut *me.warn.lock().unwrap());
            // The fake seam opens no cache: what a directory does to a switch is tested
            // against a real one.
            Ok(Connected {
                session,
                warnings,
                cache_dir: None,
                restored: None,
            })
        })
    }

    fn remove(&self, name: &str) -> anyhow::Result<Vec<String>> {
        anyhow::ensure!(name != ENV_ROW, "cannot remove the environment target");
        self.0.rows.lock().unwrap().retain(|r| r.name != name);
        Ok(vec![])
    }
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle(Key::Char(c));
    }
}

#[tokio::test]
async fn empty_state_and_rows() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![]);
    let app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    assert_eq!(app.mode, Mode::Contexts);
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("contexts_empty", app.snapshot(100, 17).unwrap()));
    let frame = app.snapshot(100, 17).unwrap();
    assert!(
        frame.contains("No contexts. Press a to add one, or run nutsh ctx add."),
        "{frame}"
    );
    // Nothing polls without a session, so the status line neither claims to be syncing nor
    // advertises a table's keys.
    assert!(!frame.contains("syncing"), "{frame}");
    assert!(!frame.contains("⏎ drill"), "{frame}");

    fake.rows
        .lock()
        .unwrap()
        .extend([row("lab", true, true), row("prod", false, false)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    settings.bind(|| insta::assert_snapshot!("contexts_rows", app.snapshot(100, 17).unwrap()));
    let frame = app.snapshot(100, 17).unwrap();
    assert!(frame.contains("* lab") && frame.contains("prod"), "{frame}");
    // PASSWORD is the last of the six columns; the menu is not drawn over this screen, so the
    // 100-column body the design sizes them for is all of it.
    assert!(frame.contains("stored"), "{frame}");

    // A reload keeps the cursor on the row it was on, not on its old index: the file may have
    // grown a context above it since.
    app.handle(Key::Char('j'));
    assert_eq!(app.screen.current().map(|r| r.name.as_str()), Some("prod"));
    fake.rows
        .lock()
        .unwrap()
        .insert(0, row("added-elsewhere", false, true));
    app.handle(Key::Ctrl('r'));
    assert_eq!(app.screen.rows.len(), 3);
    assert_eq!(app.screen.current().map(|r| r.name.as_str()), Some("prod"));
}

/// The `(env)` row is a target like any other on screen, and the only one `d` refuses.
#[tokio::test]
async fn the_environment_row_is_drawn_and_cannot_be_removed() {
    let pc = MockPc::builder().start().await;
    let mut env = row(ENV_ROW, false, true);
    env.username = "root".into();
    let fake = FakeContexts::new(&pc, vec![env, row("lab", true, true)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    let frame = app.snapshot(100, 17).unwrap();
    assert!(frame.contains("(env)") && frame.contains("root"), "{frame}");
    app.handle(Key::Char('k'));
    app.handle(Key::Char('d'));
    let frame = app.snapshot(100, 17).unwrap();
    assert!(
        frame.contains("the environment target is not saved"),
        "{frame}"
    );
    assert!(app.screen.confirm_remove.is_none());
    assert_eq!(app.screen.rows.len(), 2);
}

#[tokio::test]
async fn enter_connects_with_a_stored_password_and_opens_the_table() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Enter);
    assert!(app.screen.pending, "connecting");
    assert!(
        app.snapshot(100, 17)
            .unwrap()
            .contains("connecting to pc.lab.example")
    );
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Contexts, "keys are ignored while pending");
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(app.view().unwrap().key.kind.id, "vmm.ahv.config.Vm");
    assert!(matches!(
        fake.requests.lock().unwrap()[0],
        ConnectRequest::Stored { .. }
    ));
}

#[tokio::test]
async fn a_row_without_a_password_opens_the_login_field() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, false)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Form);
    type_str(&mut app, "wrong");
    let frame = app.snapshot(100, 17).unwrap();
    assert!(
        frame.contains("*****") && !frame.contains("wrong"),
        "masked: {frame}"
    );
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    // The field stays open with what was typed: a rejected password is corrected, not retyped
    // from an empty screen.
    assert_eq!(app.mode, Mode::Form);
    let frame = app.snapshot(100, 17).unwrap();
    assert!(
        frame.contains("rejected") && frame.contains("*****"),
        "{frame}"
    );
    for _ in 0..5 {
        app.handle(Key::Backspace);
    }
    type_str(&mut app, "secret");
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Table);
    assert!(app.screen.prompt.is_none(), "the field closed behind it");
    assert!(fake.rows.lock().unwrap()[0].has_password);
}

#[tokio::test]
async fn add_form_validates_then_adds_and_connects() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Form);
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("add_form_empty", app.snapshot(100, 23).unwrap()));
    // name taken
    type_str(&mut app, "lab");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Form);
    assert!(app.snapshot(100, 23).unwrap().contains("already exists"));
    for _ in 0..3 {
        app.handle(Key::Backspace);
    }
    type_str(&mut app, "new");
    app.handle(Key::Tab);
    type_str(&mut app, "pc.new.example");
    app.handle(Key::Tab); // port, prefilled 9440
    for _ in 0..4 {
        app.handle(Key::Backspace);
    }
    type_str(&mut app, "abc");
    app.handle(Key::Enter);
    assert!(
        app.snapshot(100, 23)
            .unwrap()
            .contains("port must be a number")
    );
    for _ in 0..3 {
        app.handle(Key::Backspace);
    }
    type_str(&mut app, "9440");
    app.handle(Key::Tab);
    type_str(&mut app, "admin");
    app.handle(Key::Tab);
    type_str(&mut app, "secret");
    app.handle(Key::Tab); // cluster, optional
    app.handle(Key::Tab); // insecure
    app.handle(Key::Char(' '));
    app.handle(Key::Tab); // read-only
    settings.bind(|| insta::assert_snapshot!("add_form_filled", app.snapshot(100, 23).unwrap()));
    app.handle(Key::Enter);
    assert!(app.screen.pending);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Table);
    assert!(app.screen.prompt.is_none(), "the form closed behind it");
    let req = fake.requests.lock().unwrap().last().cloned().unwrap();
    match req {
        ConnectRequest::Add(new) => {
            assert_eq!(
                (
                    new.name.as_str(),
                    new.host.as_str(),
                    new.username.as_str(),
                    new.insecure,
                    new.readonly
                ),
                ("new", "pc.new.example", "admin", true, false)
            );
            assert_eq!(new.port, 9440);
        }
        other => panic!("{other:?}"),
    }
}

/// A connect that fails must cost nothing that was typed: eight fields are too many to ask
/// for again because one of them was wrong.
#[tokio::test]
async fn a_rejected_password_keeps_the_add_form() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Char('a'));
    type_str(&mut app, "new");
    app.handle(Key::Tab);
    type_str(&mut app, "pc.new.example");
    app.handle(Key::Tab); // port, prefilled 9440
    app.handle(Key::Tab);
    type_str(&mut app, "admin");
    app.handle(Key::Tab);
    type_str(&mut app, "wrong");
    app.handle(Key::Enter);
    assert!(app.screen.pending);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Form);
    let Some(Prompt::Add(form)) = app.screen.prompt.as_ref() else {
        panic!("the form is still open")
    };
    assert_eq!(
        (
            form.name.as_str(),
            form.host.as_str(),
            form.port.as_str(),
            form.username.as_str(),
            form.password.as_str()
        ),
        ("new", "pc.new.example", "9440", "admin", "wrong")
    );
    assert!(
        form.error.as_deref().unwrap_or("").contains("rejected"),
        "{form:?}"
    );
    let frame = app.snapshot(100, 23).unwrap();
    assert!(
        frame.contains("rejected") && frame.contains("new"),
        "{frame}"
    );
    // Only the password needs correcting.
    for _ in 0..5 {
        app.handle(Key::Backspace);
    }
    type_str(&mut app, "secret");
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Table);
}

/// A config file that cannot be read is not an empty one: the error is shown, and the invitation
/// to add a context is not.
#[tokio::test]
async fn a_broken_config_shows_its_error_until_it_is_fixed() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    *fake.broken.lock().unwrap() = Some("config.toml: expected `=` at line 3".into());
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    let frame = app.snapshot(100, 15).unwrap();
    assert!(frame.contains("expected `=` at line 3"), "{frame}");
    assert!(!frame.contains("Press a to add one"), "{frame}");
    assert!(app.screen.rows.is_empty());

    *fake.broken.lock().unwrap() = None;
    app.handle(Key::Ctrl('r'));
    let frame = app.snapshot(100, 15).unwrap();
    assert!(
        !frame.contains("expected `=`"),
        "the error is gone: {frame}"
    );
    assert_eq!(app.screen.rows.len(), 1);
}

/// The connection is real, so what could not be done on the way is shown next to it.
#[tokio::test]
async fn warnings_survive_a_connect() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    fake.warn
        .lock()
        .unwrap()
        .push("connected, but the password could not be stored: no backend".into());
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Table);
    assert!(
        app.status
            .as_deref()
            .unwrap_or("")
            .contains("could not be stored")
    );
    assert!(
        app.screen
            .message
            .as_deref()
            .unwrap_or("")
            .contains("could not be stored"),
        "and behind the table, where a reload has not cleared it yet"
    );
}

#[tokio::test]
async fn remove_asks_for_confirmation() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true), row("prod", false, false)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Char('j'));
    app.handle(Key::Char('d'));
    assert!(app.snapshot(100, 17).unwrap().contains("remove prod? y/n"));
    app.handle(Key::Char('n'));
    assert_eq!(app.screen.rows.len(), 2);
    app.handle(Key::Char('d'));
    app.handle(Key::Char('y'));
    assert_eq!(app.screen.rows.len(), 1);
    assert_eq!(app.screen.selected, 0, "the cursor stays where the row was");
    assert!(app.snapshot(100, 17).unwrap().contains("removed prod"));
}

/// With no box open there is nowhere to put the reason but under the rows, where it replaces
/// the `connecting to …` it answers.
#[tokio::test]
async fn a_failed_connect_from_the_rows_says_so_under_them() {
    let pc = MockPc::builder()
        .credentials("admin", "other")
        .start()
        .await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Contexts);
    assert!(app.live.is_none(), "nothing to connect to");
    assert!(app.screen.prompt.is_none());
    let frame = app.snapshot(100, 17).unwrap();
    assert!(frame.contains("* lab"), "{frame}");
    assert!(!frame.contains("connecting to"), "{frame}");
    // The whole message, not just its presence in a field: the frame is where the user reads
    // it, and the message area cuts what it cannot fit.
    let message = app.screen.message.clone().expect("a reason");
    let head: String = message.chars().take(40).collect();
    assert!(frame.contains(&head), "{message}\nin\n{frame}");
}

/// Without a session there is no table to go back to and no command to run.
#[tokio::test]
async fn a_disconnected_screen_has_nowhere_to_go_back_to() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Contexts);
    app.handle(Key::Char(':'));
    assert_eq!(app.mode, Mode::Contexts, "no session, no palette");
    assert!(app.palette.is_none());
    assert!(!app.should_quit());
    app.handle(Key::Char('q'));
    assert!(app.should_quit());
}

#[tokio::test]
async fn ctx_command_switches_sessions_and_reopens_the_kind() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true), row("other", false, true)]);
    let session = common::session(&pc).await;
    let mut app = App::open(session, Fake::boxed(&fake), "cluster", common::config()).unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx other");
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(
        app.live.as_ref().unwrap().session.context.as_deref(),
        Some("other")
    );
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "clustermgmt.config.Cluster"
    );

    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Contexts);
    app.handle(Key::Esc);
    assert_eq!(
        app.mode,
        Mode::Table,
        "esc returns to the table when connected"
    );
}

/// A session opened from the command line completes `:ctx ` from the rows read at startup:
/// the names are not typed from memory, and the popup is titled for what it lists.
#[tokio::test]
async fn ctx_completes_from_the_rows_read_at_startup() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true), row("other", false, true)]);
    let session = common::session(&pc).await;
    let mut app = App::open(session, Fake::boxed(&fake), "cluster", common::config()).unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx ");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(
        frame.contains(" contexts ") && frame.contains("lab") && frame.contains("other"),
        "{frame}"
    );
    type_str(&mut app, "oth");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(!frame.contains("lab   "), "narrowed: {frame}");
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(
        app.live.as_ref().unwrap().session.context.as_deref(),
        Some("other")
    );
}

/// `:ctx nothing` says so and leaves the screen it was called from alone.
#[tokio::test]
async fn ctx_command_reports_a_name_it_does_not_know() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    let session = common::session(&pc).await;
    let mut app = App::open(session, Fake::boxed(&fake), "cluster", common::config()).unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx nope");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(app.status.as_deref(), Some("no context named nope"));
}

/// `:ctx NAME` on a context with no stored password asks for one, and says why inside the box:
/// the box covers the rows, so a sentence under them would be wiped by the one that needs it.
#[tokio::test]
async fn ctx_command_asks_for_a_password_it_does_not_have() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(
        &pc,
        vec![row("lab", true, true), row("other", false, false)],
    );
    let session = common::session(&pc).await;
    let mut app = App::open(session, Fake::boxed(&fake), "cluster", common::config()).unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx other");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Form);
    let frame = app.snapshot(100, 19).unwrap();
    assert!(frame.contains("password for other"), "{frame}");
    assert!(
        frame.contains("no stored password for other; run `nutsh ctx login other` or type it here"),
        "{frame}"
    );
    // Typing answers it; the box says nothing more until the connect does.
    type_str(&mut app, "secret");
    let frame = app.snapshot(100, 19).unwrap();
    assert!(!frame.contains("no stored password"), "{frame}");
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(
        app.live.as_ref().unwrap().session.context.as_deref(),
        Some("other")
    );
}

/// The palette opens over the Contexts screen too, and `esc` goes back to the rows rather than
/// to the table underneath them.
#[tokio::test]
async fn the_palette_returns_to_the_screen_it_was_opened_over() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true)]);
    let session = common::session(&pc).await;
    let mut app = App::open(session, Fake::boxed(&fake), "cluster", common::config()).unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Contexts);
    app.handle(Key::Char(':'));
    assert_eq!(app.mode, Mode::Command);
    let frame = app.snapshot(100, 27).unwrap();
    assert!(
        frame.contains("* lab") && frame.contains("FLAGS              PASSWORD"),
        "the rows, not the table, stay behind the palette: {frame}"
    );
    // The header describes the screen on show, not the table it was opened over.
    assert!(
        frame.contains("Kind:       Contexts") && frame.contains("Count:      1 context"),
        "{frame}"
    );
    assert!(!frame.contains("⏎ drill"), "{frame}");
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Contexts);
}

/// A Prism Central that serves nothing the app can open leaves the user on the screen that can
/// pick another one, not on an empty table.
#[tokio::test]
async fn a_switch_to_a_pc_that_serves_nothing_lands_back_on_the_screen() {
    let pc = MockPc::builder().start().await;
    let bare = MockPc::builder()
        .unavailable_namespace("clustermgmt")
        .start()
        .await;
    let fake = FakeContexts::new(&pc, vec![row("lab", true, true), row("other", false, true)]);
    let session = common::session(&pc).await;
    let mut app = App::open(session, Fake::boxed(&fake), "cluster", common::config()).unwrap();
    common::settle(&mut app).await;
    fake.point_at(&bare);
    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx other");
    app.handle(Key::Enter);
    common::finish_connect(&mut app).await;
    assert_eq!(app.mode, Mode::Contexts);
    assert!(app.live.is_some(), "the session itself is fine");
    assert!(app.view().is_none(), "there is nothing to show in it");
    let frame = app.snapshot(100, 19).unwrap();
    assert!(frame.contains("namespace not served"), "{frame}");
}

/// A list longer than the screen has to scroll, and what the screen says about it has to stay
/// on the screen: `d` below the fold asks a question the user answers `y` to without ever
/// having read it.
#[tokio::test]
async fn a_long_list_scrolls_and_keeps_its_message_in_the_frame() {
    let pc = MockPc::builder().start().await;
    let rows: Vec<ContextRow> = (0..30)
        .map(|i| row(&format!("ctx{i:02}"), i == 0, true))
        .collect();
    let fake = FakeContexts::new(&pc, rows);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    for _ in 0..15 {
        app.handle(Key::Char('j'));
    }
    assert_eq!(app.screen.current().map(|r| r.name.as_str()), Some("ctx15"));
    let frame = app.snapshot(100, 21).unwrap();
    // `>` then the two-cell current marker: the row no longer starts the line, since the menu
    // is drawn beside the screen.
    assert!(
        frame.contains(">  ctx15"),
        "the cursor is in the frame: {frame}"
    );

    app.handle(Key::Char('d'));
    let frame = app.snapshot(100, 21).unwrap();
    assert!(frame.contains("remove ctx15? y/n"), "{frame}");
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("contexts_scrolled", frame));
    app.handle(Key::Char('n'));

    app.handle(Key::Char('G'));
    assert_eq!(app.screen.current().map(|r| r.name.as_str()), Some("ctx29"));
    let frame = app.snapshot(100, 21).unwrap();
    assert!(
        frame.contains(">  ctx29"),
        "G reaches the last row and drags the window with it: {frame}"
    );
    assert!(!frame.contains("ctx00"), "the top scrolled off: {frame}");
    app.handle(Key::Char('g'));
    let frame = app.snapshot(100, 21).unwrap();
    assert!(
        frame.contains(">* ctx00"),
        "g comes back to the top: {frame}"
    );
    assert!(frame.contains("more)"), "and says what is below: {frame}");
}

/// A mistyped host costs a keystroke rather than the rest of the field: the add form's six text
/// fields carry a cursor, and `ctrl-w` takes a word.
#[tokio::test]
async fn the_add_form_edits_in_the_middle_of_a_field() {
    let pc = MockPc::builder().start().await;
    let fake = FakeContexts::new(&pc, vec![]);
    let mut app = App::disconnected(Fake::boxed(&fake), "vm", None, None, common::config());
    app.handle(Key::Char('a'));
    type_str(&mut app, "new");
    app.handle(Key::Tab);
    type_str(&mut app, "pc.nw.example");
    // Back over `w.example` and put the missing letter in.
    for _ in 0..9 {
        app.handle(Key::Left);
    }
    app.handle(Key::Char('e'));
    let Some(Prompt::Add(form)) = app.screen.prompt.as_ref() else {
        panic!("the form is open")
    };
    assert_eq!(form.host.as_str(), "pc.new.example");
    assert_eq!(
        form.name.as_str(),
        "new",
        "and the other fields are untouched"
    );

    // `ctrl-w` takes the word before the cursor, wherever it stands.
    app.handle(Key::End);
    app.handle(Key::Ctrl('w'));
    let Some(Prompt::Add(form)) = app.screen.prompt.as_ref() else {
        panic!("the form is open")
    };
    assert_eq!(form.host.as_str(), "");

    // A checkbox has no text, so an editing key on one is a no-op rather than a special case.
    for _ in 0..5 {
        app.handle(Key::Tab);
    }
    app.handle(Key::Ctrl('w'));
    app.handle(Key::Left);
    let Some(Prompt::Add(form)) = app.screen.prompt.as_ref() else {
        panic!("the form is open")
    };
    assert_eq!(form.current(), contexts::Field::Insecure);
    assert!(!form.insecure, "and it did not toggle either");
}
