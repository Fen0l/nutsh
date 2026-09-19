use std::process::{Command, Output};

use nutsh_mockpc::MockPc;

/// Runs the binary against `pc`. The blocking child-process wait goes through
/// `spawn_blocking` so it never occupies a runtime worker: that is what keeps the mock's
/// accept loop polled while the child talks to it, rather than the `worker_threads = 2`.
async fn run_check(pc: &MockPc, password: &str) -> Output {
    run_check_with(pc, password, &[]).await
}

/// `run_check` plus flags a single test cares about.
async fn run_check_with(pc: &MockPc, password: &str, extra: &[&str]) -> Output {
    let host = pc.host();
    let port = pc.port().to_string();
    let password = password.to_string();
    let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
    // A config path inside an empty temp dir: every command path reads the file for its
    // `[[guardrails]]` before connecting, and the developer's own must not be the one read.
    let dir = tempfile::tempdir().unwrap();
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_nutsh"))
            .args([
                "--host",
                &host,
                "--port",
                &port,
                "--username",
                "admin",
                "--check",
                "--plain-http",
            ])
            .args(&extra)
            .env("NUTSH_CONFIG", dir.path().join("config.toml"))
            .env("NUTSH_PASSWORD", password)
            .output()
            .expect("run nutsh")
    })
    .await
    .expect("join")
}

fn row<'a>(stdout: &'a str, namespace: &str) -> Vec<&'a str> {
    stdout
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|cols| cols.first() == Some(&namespace))
        .unwrap_or_else(|| panic!("no row for {namespace} in:\n{stdout}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_reports_every_namespace_ok() {
    let pc = MockPc::builder().start().await;
    let out = run_check(&pc, "secret").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(row(&stdout, "vmm")[..3], ["vmm", "v4.3", "ok"]);
    let n = nutsh_catalog::NAMESPACES.len();
    assert!(
        stdout.contains(&format!("{n}/{n} namespaces available")),
        "{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_flags_an_unavailable_namespace_and_exits_2() {
    let pc = MockPc::builder()
        .unavailable_namespace("files")
        .start()
        .await;
    let out = run_check(&pc, "secret").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{stdout}");
    let files = row(&stdout, "files");
    assert_eq!(files[1], "-", "no version answered");
    assert!(files[2].starts_with("unavailable"), "{files:?}");
    assert!(stdout.contains("not served at v4.0"), "{stdout}");
    let n = nutsh_catalog::NAMESPACES.len();
    assert!(
        stdout.contains(&format!("{}/{n} namespaces available", n - 1)),
        "{stdout}"
    );
}

/// `tenancy` exists only as an alpha spec, so no Prism Central serves it: its row is shown
/// but a healthy PC still exits 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preview_only_namespace_does_not_fail_the_check() {
    let pc = MockPc::builder()
        .unavailable_namespace("tenancy")
        .start()
        .await;
    let out = run_check(&pc, "secret").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        row(&stdout, "tenancy")[2].starts_with("unavailable"),
        "{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_with_wrong_password_exits_3() {
    let pc = MockPc::builder()
        .credentials("admin", "other")
        .start()
        .await;
    let out = run_check(&pc, "secret").await;
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("authentication failed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_http_refuses_non_loopback_host() {
    let dir = tempfile::tempdir().unwrap();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_nutsh"))
            .args([
                "--host",
                "10.0.0.1",
                "--username",
                "a",
                "--plain-http",
                "--check",
            ])
            .env("NUTSH_CONFIG", dir.path().join("config.toml"))
            .env("NUTSH_PASSWORD", "x")
            .output()
            .expect("run nutsh")
    })
    .await
    .expect("join");
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("loopback"));
}

/// No host and no context: the error names `--host` as the way out. `NUTSH_CONFIG` points at a
/// path inside an empty temp dir so the developer's own config cannot supply a context.
#[test]
fn check_without_host_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nutsh"))
        .args(["--check", "--username", "a"])
        .env("NUTSH_CONFIG", dir.path().join("config.toml"))
        .env_remove("NUTSH_HOST")
        .env_remove("NUTSH_CONTEXT")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--host"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_prints_a_header_with_host_and_no_context() {
    let pc = MockPc::builder().start().await;
    let out = run_check(&pc, "secret").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let header = stdout.lines().next().unwrap_or("");
    assert!(header.starts_with("context -"), "{header}");
    assert!(header.contains(&pc.host()), "{header}");
    assert!(
        header.contains("cluster -") && header.contains("read-write"),
        "{header}"
    );
    assert!(!header.contains("[insecure]"), "{header}");
}

/// TLS verification off is a property of the session, so the header says so. The mock speaks
/// plain HTTP either way: `--insecure` only flips the flag the header reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_marks_an_insecure_session() {
    let pc = MockPc::builder().start().await;
    let out = run_check_with(&pc, "secret", &["--insecure"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let header = stdout.lines().next().unwrap_or("");
    assert!(header.ends_with("read-write [insecure]"), "{header}");
}

/// `--username` overrides the context's user, so the password must be looked up under the
/// account that is actually sent. Storing one account's password under `other@host:port` is what makes
/// this run work; the context's own `admin` key is never consulted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn username_override_uses_its_own_stored_password() {
    let pc = MockPc::builder()
        .credentials("other", "secret")
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[contexts.lab]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n",
            pc.host(),
            pc.port()
        ),
    )
    .unwrap();
    let run = {
        let config = config.clone();
        let state = dir.path().to_path_buf();
        move || {
            Command::new(env!("CARGO_BIN_EXE_nutsh"))
                .args([
                    "--check",
                    "--plain-http",
                    "--context",
                    "lab",
                    "--username",
                    "other",
                ])
                .env("NUTSH_CONFIG", &config)
                .env("XDG_STATE_HOME", &state)
                .env("NUTSH_KEYRING", "0")
                .env_remove("NUTSH_PASSWORD")
                .env_remove("NUTSH_HOST")
                .env_remove("NUTSH_CONTEXT")
                .stdin(std::process::Stdio::null())
                .output()
                .expect("run nutsh")
        }
    };

    // Nothing stored for `other` yet, and no terminal to prompt at.
    let out = tokio::task::spawn_blocking(run.clone())
        .await
        .expect("join");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("run `nutsh ctx login lab`"), "{stderr}");

    let secrets = dir.path().join("nutsh").join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(
        secrets.join(format!("other@{}_{}", pc.host(), pc.port())),
        "secret",
    )
    .unwrap();

    let out = tokio::task::spawn_blocking(run).await.expect("join");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout
            .lines()
            .next()
            .unwrap_or("")
            .starts_with("context lab"),
        "{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_shows_the_pc_version_and_stepped_down_namespaces() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    let out = run_check(&pc, "secret").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let header = stdout.lines().next().unwrap_or("");
    assert!(
        header.contains(&format!("{} pc.2024.3", pc.host())),
        "{header}"
    );
    assert_eq!(
        row(&stdout, "vmm"),
        ["vmm", "v4.1", "(catalog", "v4.3)", "ok"]
    );
    assert_eq!(
        row(&stdout, "clustermgmt")[..3],
        ["clustermgmt", "v4.3", "ok"]
    );
}

/// The PC version comes from the `prism` namespace, so a PC that does not serve it still gets
/// a header - just without a version, rather than with a placeholder or an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_header_omits_the_pc_version_when_it_cannot_be_read() {
    let pc = MockPc::builder()
        .unavailable_namespace("prism")
        .start()
        .await;
    let out = run_check(&pc, "secret").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{stdout}");
    let header = stdout.lines().next().unwrap_or("");
    assert!(header.contains(&pc.host()), "{header}");
    assert!(!header.contains("pc."), "{header}");
}

/// `--check -c lab,dr` probes both, one table each, and the exit code is the worse of the two.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_with_two_contexts_prints_a_table_for_each() {
    let lab = MockPc::builder().start().await;
    let dr = MockPc::builder()
        .unavailable_namespace("volumes")
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[contexts.lab]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n\n[contexts.dr]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n",
            lab.host(),
            lab.port(),
            dr.host(),
            dr.port()
        ),
    )
    .unwrap();
    let state = dir.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_nutsh"))
            .args(["--check", "--plain-http", "-c", "lab,dr"])
            .env("NUTSH_CONFIG", &config)
            .env("XDG_STATE_HOME", &state)
            .env("NUTSH_KEYRING", "0")
            .env("NUTSH_PASSWORD", "secret")
            .env_remove("NUTSH_HOST")
            .env_remove("NUTSH_CONTEXT")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run nutsh")
    })
    .await
    .expect("join");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("context lab"), "{stdout}");
    assert!(stdout.contains("context dr"), "{stdout}");
    assert_eq!(
        stdout.matches("NAMESPACE").count(),
        2,
        "one table per context: {stdout}"
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "dr is missing a namespace, so the run says so: {stdout}"
    );
}
