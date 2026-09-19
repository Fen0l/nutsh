//! `nutsh [KIND] --snapshot`: one headless frame, and the exit codes of a startup that cannot
//! reach a table.

use std::process::{Command, Output, Stdio};

use nutsh_mockpc::MockPc;

fn nutsh(args: &[&str], config: &std::path::Path, env: &[(&str, String)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nutsh"));
    cmd.args(args).stdin(Stdio::null());
    // Before anything is set, not after: every `NUTSH_*` clap reads is a flag by another
    // name, so a developer's own environment must not reach the child - `NUTSH_PORT=1` in the
    // shell that runs `cargo test` would otherwise send every one of these tests at the wrong
    // port.
    scrub(&mut cmd);
    cmd.env("NUTSH_CONFIG", config)
        .env("XDG_STATE_HOME", config.parent().unwrap().join("state"))
        .env("NUTSH_KEYRING", "0");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run nutsh")
}

/// Drop every `NUTSH_*` variable this process inherited, whatever it is called. Listing them
/// would go stale the next time one is added.
fn scrub(cmd: &mut Command) {
    for (k, _) in std::env::vars() {
        if k.starts_with("NUTSH_") {
            cmd.env_remove(k);
        }
    }
}

async fn run(
    args: Vec<String>,
    config: std::path::PathBuf,
    env: Vec<(&'static str, String)>,
) -> Output {
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        nutsh(&args, &config, &env)
    })
    .await
    .expect("join")
}

fn config_with_lab(pc: &MockPc, dir: &std::path::Path, stored: bool) -> std::path::PathBuf {
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "current_context = \"lab\"\n[contexts.lab]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n",
            pc.host(),
            pc.port()
        ),
    )
    .unwrap();
    if stored {
        let secrets = dir.join("state").join("nutsh").join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::write(
            secrets.join(format!("admin@{}_{}", pc.host(), pc.port())),
            "secret",
        )
        .unwrap();
    }
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_of_the_vm_table_by_host_flags() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        vec![
            "vm".into(),
            "--snapshot".into(),
            "--plain-http".into(),
            "--host".into(),
            pc.host(),
            "--port".into(),
            pc.port().to_string(),
            "--username".into(),
            "admin".into(),
        ],
        dir.path().join("config.toml"),
        vec![("NUTSH_PASSWORD", "secret".into())],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("NAME") && stdout.contains("POWER"),
        "{stdout}"
    );
    for name in ["web-01", "web-02", "db-01"] {
        assert!(stdout.contains(name), "{stdout}");
    }
    assert_eq!(stdout.lines().count(), 40, "default size is 120x40");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_with_a_context_and_a_size() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec![
            "--snapshot".into(),
            "--size".into(),
            "100x12".into(),
            "--plain-http".into(),
            "--readonly".into(),
        ],
        config,
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Twelve rows folds the header, so the context and the session's mode are on the one line
    // the box was, and nothing but the sidebar and the table is under it. This is the real
    // default reaching a real frame: the TUI's own suites pin the full header, and only the
    // binary reads `header = "auto"` out of a config file that never mentions the key.
    let header = stdout.lines().next().unwrap();
    assert!(
        header.contains("lab") && header.contains("[read-only]"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("Context:"),
        "the box is folded away, not drawn small: {stdout}"
    );
    assert_eq!(stdout.lines().count(), 12);
}

/// `[KIND]` names a feature page as readily as a kind, and `--snapshot` waits for every one
/// of its panes before it prints the frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_of_the_disaster_recovery_page() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec![
            "disaster-recovery".into(),
            "--snapshot".into(),
            "--size".into(),
            "140x40".into(),
            "--plain-http".into(),
        ],
        config,
        vec![],
    )
    .await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let frame = String::from_utf8_lossy(&out.stdout);
    for title in [
        "Registered Prism Centrals",
        "Protection Policies",
        "Recovery Plans",
        "Recent DR jobs",
    ] {
        assert!(frame.contains(title), "{title}:\n{frame}");
    }
}

/// A frame with no colour at all still lays out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_color_still_renders_a_frame() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec!["vm".into(), "--snapshot".into(), "--plain-http".into()],
        config,
        vec![("NUTSH_COLOR", "none".to_string())],
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("web-01"));
}

/// A config naming a skin nothing knows still starts, and warns on stderr.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_skin_warns_and_starts() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let text = std::fs::read_to_string(&config).unwrap();
    // Appended, not prepended: `[skin]` above `current_context` would take that key into the
    // skin table.
    std::fs::write(
        &config,
        format!("{text}\n[skin]\nname = \"no-such-skin\"\n"),
    )
    .unwrap();
    let out = run(
        vec!["vm".into(), "--snapshot".into(), "--plain-http".into()],
        config,
        vec![],
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no-such-skin"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_and_unreachable_kinds_exit_3() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec!["nope".into(), "--snapshot".into(), "--plain-http".into()],
        config.clone(),
        vec![],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown kind"));
    // `nics` resolves only to the VM's NICs, a child kind that cannot open on its own.
    let out = run(
        vec!["nics".into(), "--snapshot".into(), "--plain-http".into()],
        config,
        vec![],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("open from Virtual Machines"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_without_a_usable_target_shows_the_screen_or_says_why() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        vec!["--snapshot".into(), "--size".into(), "100x8".into()],
        dir.path().join("absent.toml"),
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("No contexts. Press a to add one"),
        "{stdout}"
    );

    // A context nobody has a password for is a login box interactively, but under `--snapshot`
    // nobody is going to type into it: the run asked for a session and did not get one.
    let config = config_with_lab(&pc, dir.path(), false);
    let out = run(
        vec![
            "--snapshot".into(),
            "--size".into(),
            "100x8".into(),
            "--plain-http".into(),
        ],
        config,
        vec![],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no stored password for lab"), "{stderr}");
    assert!(stderr.contains("ctx login lab"), "{stderr}");
    // On stderr there is no "here" to type into, so the other way out is the one a next run
    // can take.
    assert!(stderr.contains("set NUTSH_PASSWORD"), "{stderr}");
    assert!(!stderr.contains("type it here"), "{stderr}");
    assert!(
        out.stdout.is_empty(),
        "no frame is printed for a failed run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_with_a_failing_named_target_exits_3() {
    let pc = MockPc::builder()
        .credentials("admin", "other")
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec!["--snapshot".into(), "--plain-http".into()],
        config,
        vec![],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ctx login lab"));
}

/// `--readonly` is a property of the run, not of the context, so it has to reach a session the
/// Contexts screen opens as well as one the command line does. The screen is where that is
/// visible before any connect: the row it offers says `read-only`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readonly_marks_the_rows_the_contexts_screen_offers() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    // No `current_context`, so nothing is selected and nothing is connected to: the screen
    // lists its rows with nothing over them, which is where the flag has to show.
    std::fs::write(
        &config,
        "[contexts.lab]\nhost = \"10.0.0.1\"\nusername = \"admin\"\n",
    )
    .unwrap();
    let out = run(
        vec![
            "--snapshot".into(),
            "--size".into(),
            "120x10".into(),
            "--readonly".into(),
        ],
        config,
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let row = stdout
        .lines()
        .find(|l| l.contains("lab"))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(row.contains("read-only"), "{stdout}");
}

/// A config file that will not parse is a failed run, not an empty screen: `--snapshot` says
/// so on stderr and exits 3 rather than printing a frame that hides the problem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_corrupt_config_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "current_context = [\n").unwrap();
    let out = run(vec!["--snapshot".into()], config, vec![]).await;
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("config.toml"), "{stderr}");
}

/// A `current_context` naming a context that is not there is the command line asking for
/// something it did not get, so it fails; but the file still loads, so the contexts that *are*
/// there stay reachable - that is what makes the screen able to fix it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_current_context_exits_3_and_keeps_the_other_rows() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("current_context = \"lab\"", "current_context = \"gone\"");
    std::fs::write(&config, text).unwrap();
    let out = run(
        vec!["--snapshot".into(), "--plain-http".into()],
        config.clone(),
        vec![],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown context gone"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `lab` is still listed, and still connectable by name.
    let out = run(
        vec![
            "--snapshot".into(),
            "--plain-http".into(),
            "--context".into(),
            "lab".into(),
        ],
        config,
        vec![],
    )
    .await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Context:    lab"));
}

/// A `--host` target with no password has no row to log in from, so there is nowhere for the
/// screen to take one: the run fails and says what would have let it through.
/// Nothing is connected to here - the run stops before the socket - so there is no mock: the
/// host only has to be a name the target can be built from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_target_without_a_password_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        vec![
            "--snapshot".into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--username".into(),
            "admin".into(),
        ],
        dir.path().join("absent.toml"),
        vec![],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NUTSH_PASSWORD"), "{stderr}");
    assert!(stderr.contains("admin@127.0.0.1"), "{stderr}");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Exit criterion 1 against the mock, where the warm-up and the stats cycle always arrive: the
/// frame carries a resolved cluster name, no eight-hex-digit stub, and numbers in the stats
/// block. Against a live Prism Central the same wait is non-fatal, which is why this is where
/// the determinism is pinned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_snapshot_resolves_names_and_fills_the_stats_block() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec![
            "vm".into(),
            "--snapshot".into(),
            "--size".into(),
            "120x30".into(),
            "--plain-http".into(),
        ],
        config,
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("lab-cluster"), "{stdout}");
    assert!(stdout.contains("ahv-node-1"), "{stdout}");
    assert!(
        !stdout.contains("0006158a"),
        "no stub in the frame: {stdout}"
    );
    assert!(stdout.contains("clusters 1"), "{stdout}");
    assert!(stdout.contains("vms 3"), "{stdout}");
}

/// And the second wait cannot fail a snapshot: a Prism Central that never answers the warm-up
/// still gets a frame and exit 0, with dashes where the numbers would be.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_snapshot_still_exits_0_when_the_warm_up_never_answers() {
    // `clustermgmt`, `networking`, `prism`, `iam` and `vmm` carry the warm set and the
    // counters; the VM list itself is answered by `vmm`, which stays served, so the first
    // (fatal) wait succeeds and only the second (non-fatal) one times out.
    let pc = MockPc::builder()
        .unavailable_namespace("clustermgmt")
        .unavailable_namespace("networking")
        .unavailable_namespace("prism")
        .unavailable_namespace("iam")
        .unavailable_namespace("monitoring")
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec![
            "vm".into(),
            "--snapshot".into(),
            "--size".into(),
            "120x30".into(),
            "--plain-http".into(),
        ],
        config,
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the second wait is non-fatal; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("web-01"),
        "the rows are still there: {stdout}"
    );
    assert!(stdout.contains("clusters -"), "{stdout}");
}

/// A bare `nutsh` opens the **Dashboard**, not the VM table: the first view is the overview,
/// and a kind is what a user asks for by name. The start target is a page id or a kind id
/// resolved by the one ranking the palette uses, so no argument shape changes to say "a page".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bare_run_opens_the_dashboard_and_a_named_kind_still_opens_its_table() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    let out = run(
        vec!["--snapshot".into(), "--plain-http".into()],
        config.clone(),
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for pane in ["Clusters", "Unresolved alerts", "Running tasks"] {
        assert!(stdout.contains(pane), "the Dashboard's panes: {stdout}");
    }

    // And the VM kind still opens its table when it is asked for by name.
    let out = run(
        vec!["vm".into(), "--snapshot".into(), "--plain-http".into()],
        config,
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout.contains("POWER") && stdout.contains("web-01"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("Unresolved alerts"),
        "a table, not the page: {stdout}"
    );
}

/// The launch view cannot be hidden out from under itself. `:hide` refuses both spellings of
/// the Dashboard, and a config file edited by hand is the other door into the same set: a bare
/// run still opens the Dashboard, and the menu still holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_that_hides_the_dashboard_still_launches_on_it() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path(), true);
    // Above the `[contexts.*]` tables and below the bare keys, which is where `save` puts it.
    let text = std::fs::read_to_string(&config).unwrap();
    let (bare, tables) = text.split_once("[contexts.lab]").unwrap();
    std::fs::write(
        &config,
        format!("{bare}[nav]\nhide = [\"dashboard\", \"Dashboard\"]\n\n[contexts.lab]{tables}"),
    )
    .unwrap();
    let out = run(
        vec!["--snapshot".into(), "--plain-http".into()],
        config,
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Unresolved alerts"), "{stdout}");
    assert!(
        stdout.contains("▾ Dashboard") || stdout.contains("▸ Dashboard"),
        "and the menu still holds it: {stdout}"
    );
}

/// `--format csv|json` prints the table the frame would have drawn, as data: the same columns,
/// the same rows, names not identifiers. `text` is the frame, and is the default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_format_prints_the_table_as_csv_or_json() {
    let pc = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let base = |format: &str| {
        vec![
            "vm".to_string(),
            "--snapshot".into(),
            "--format".into(),
            format.into(),
            "--plain-http".into(),
            "--host".into(),
            pc.host(),
            "--port".into(),
            pc.port().to_string(),
            "--username".into(),
            "admin".into(),
        ]
    };
    let env = vec![("NUTSH_PASSWORD", "secret".to_string())];

    let out = run(base("csv"), dir.path().join("config.toml"), env.clone()).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let csv = String::from_utf8_lossy(&out.stdout);
    let mut lines = csv.lines();
    let header = lines.next().unwrap();
    assert!(header.starts_with("NAME,POWER,"), "{header}");
    let rows: Vec<&str> = lines.collect();
    assert_eq!(rows.len(), 3, "{csv}");
    assert!(rows.iter().any(|r| r.starts_with("web-01,ON,")), "{csv}");
    assert!(!csv.contains('╭'), "data, not a frame: {csv}");

    let out = run(base("json"), dir.path().join("config.toml"), env).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["NAME"], "web-01");
    assert_eq!(rows[0]["POWER"], "ON");
}

/// `--format` is `--snapshot`'s: alone it is refused by clap, before anything connects.
#[test]
fn format_without_snapshot_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = nutsh(
        &["vm", "--format", "csv"],
        &dir.path().join("config.toml"),
        &[],
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--snapshot"), "{stderr}");
}

/// Two contexts in the config file, both with a stored secret.
fn config_with_two(lab: &MockPc, dr: &MockPc, dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "current_context = \"lab\"\n[contexts.lab]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n\n[contexts.dr]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n",
            lab.host(),
            lab.port(),
            dr.host(),
            dr.port()
        ),
    )
    .unwrap();
    let secrets = dir.join("state").join("nutsh").join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    for pc in [lab, dr] {
        std::fs::write(
            secrets.join(format!("admin@{}_{}", pc.host(), pc.port())),
            "secret",
        )
        .unwrap();
    }
    config
}

/// `-c lab,dr` opens the session on `lab` and reads `dr` beside it: one table, both counted,
/// a CONTEXT column saying which row is whose, and the header naming both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_with_two_contexts_merges_their_tables() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder().start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_two(&lab, &dr, dir.path());
    let out = run(
        vec![
            "vm".into(),
            "--snapshot".into(),
            "--plain-http".into(),
            "-c".into(),
            "lab,dr".into(),
        ],
        config,
        vec![],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Context:    lab +dr"), "{stdout}");
    assert!(stdout.contains("Virtual Machines [6]"), "{stdout}");
    assert!(stdout.contains("CONTEXT"), "{stdout}");
    assert_eq!(
        stdout.lines().filter(|l| l.contains("web-01")).count(),
        2,
        "one row from each: {stdout}"
    );
}
