//! `:export`: the table in view, as it is drawn, to a file.

mod common;

use nutsh_mockpc::MockPc;
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
    common::settle_names(&mut app).await;
    app
}

fn command(app: &mut App, line: &str) {
    app.handle(Key::Char(':'));
    for c in line.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

/// What is exported is what is drawn: the view's columns in order, the rows in the table's
/// order, a resolved name where the table shows one, and the `/` filter respected.
#[tokio::test]
async fn export_writes_the_drawn_table_and_respects_the_filter() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&pc).await;
    let csv = dir.path().join("vms.csv");
    command(&mut app, &format!("export csv {}", csv.display()));
    assert_eq!(
        app.status.as_deref(),
        Some(format!("exported 3 rows to {}", csv.display()).as_str())
    );
    let text = std::fs::read_to_string(&csv).unwrap();
    let mut lines = text.lines();
    assert!(
        lines.next().unwrap().starts_with("NAME,POWER,CLUSTER,"),
        "{text}"
    );
    let web = lines
        .find(|l| l.starts_with("web-01,"))
        .expect("web-01 row");
    assert!(
        web.starts_with("web-01,ON,lab-cluster,ahv-node-1,"),
        "the cluster and the host by name, as the table draws them: {web}"
    );
    assert_eq!(text.lines().count(), 4, "a header and three rows: {text}");

    // JSON is the same sheet, one object per row.
    let json = dir.path().join("vms.json");
    command(&mut app, &format!("export json {}", json.display()));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 3);
    assert_eq!(v[0]["NAME"], "web-01");
    assert_eq!(v[0]["POWER"], "ON");

    // The filter narrows the export the way it narrows the frame.
    app.handle(Key::Char('/'));
    for c in "web".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    let narrowed = dir.path().join("web.csv");
    command(&mut app, &format!("export {}", narrowed.display()));
    let text = std::fs::read_to_string(&narrowed).unwrap();
    assert_eq!(
        text.lines().count(),
        3,
        "a header and the two web rows: {text}"
    );
    assert!(!text.contains("db-01"), "{text}");
}

/// Bare `:export` picks the format and the file: CSV, stamped to the second, in the working
/// directory, named after the kind. A word that is neither a format nor a path is refused
/// rather than becoming a file called `xlsx`.
#[tokio::test]
async fn a_bare_export_names_its_own_file_and_a_typo_is_refused() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    command(&mut app, "export xlsx");
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("csv or json"), "{status}");

    command(&mut app, "export");
    let status = app.status.clone().unwrap_or_default();
    let path = status
        .strip_prefix("exported 3 rows to ")
        .unwrap_or_else(|| panic!("{status}"));
    assert!(
        path.starts_with("nutsh-vm-") && path.ends_with(".csv"),
        "{path}"
    );
    assert!(std::fs::metadata(path).is_ok(), "{path} exists");
    std::fs::remove_file(path).unwrap();
}
