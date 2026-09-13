use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use nutsh_mockpc::MockPc;

struct Env {
    _dir: tempfile::TempDir,
    config: PathBuf,
    state: PathBuf,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        Env {
            config: dir.path().join("config.toml"),
            state: dir.path().join("state"),
            _dir: dir,
        }
    }

    /// The binary against this env's config and state, with every `NUTSH_*` input cleared and
    /// no stdin. Null stdin is what makes the "no terminal to prompt at" branch deterministic:
    /// otherwise a test run from a terminal inherits one and blocks on the prompt.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nutsh"));
        cmd.args(args)
            .env("NUTSH_CONFIG", &self.config)
            .env("XDG_STATE_HOME", &self.state)
            .env("NUTSH_KEYRING", "0")
            .env_remove("NUTSH_CONTEXT")
            .env_remove("NUTSH_HOST")
            .env_remove("NUTSH_USERNAME")
            .env_remove("NUTSH_PORT")
            .env_remove("NUTSH_PASSWORD")
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str], password: Option<&str>) -> Output {
        let mut cmd = self.cmd(args);
        if let Some(p) = password {
            cmd.env("NUTSH_PASSWORD", p);
        }
        cmd.output().expect("run nutsh")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args, None);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{:?}\n{}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn fails(&self, args: &[&str], password: Option<&str>) -> String {
        let out = self.run(args, password);
        assert_eq!(
            out.status.code(),
            Some(3),
            "{:?}\n{}",
            args,
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(&self.config).unwrap_or_default()
    }
}

#[test]
fn add_list_show_use_remove() {
    let e = Env::new();
    e.ok(&[
        "ctx",
        "add",
        "lab",
        "--host",
        "pc.lab",
        "--username",
        "admin",
        "--cluster",
        "prod-01",
        "--insecure",
    ]);
    let text = e.config_text();
    assert!(text.contains("current_context = \"lab\""), "{text}");
    assert!(
        text.contains("[contexts.lab]")
            && text.contains("verify_tls = false")
            && text.contains("cluster = \"prod-01\""),
        "{text}"
    );
    assert!(!text.contains("password"));

    e.ok(&[
        "ctx",
        "add",
        "prod",
        "--host",
        "pc.prod",
        "--username",
        "svc",
        "--readonly",
    ]);
    assert!(
        e.config_text().contains("current_context = \"lab\""),
        "adding a second context must not change current"
    );

    let list = e.ok(&["ctx", "list"]);
    let lab = list
        .lines()
        .find(|l| l.contains(" lab "))
        .unwrap_or_else(|| panic!("{list}"));
    assert!(lab.starts_with('*'), "{lab}");
    for col in [
        "pc.lab",
        "admin",
        "prod-01",
        "insecure",
        "read-write",
        "none",
    ] {
        assert!(lab.contains(col), "missing {col} in {lab}");
    }
    let prod = list.lines().find(|l| l.contains(" prod ")).unwrap();
    assert!(
        prod.contains("read-only") && prod.contains("verify"),
        "{prod}"
    );

    let show = e.ok(&["ctx", "show", "prod"]);
    assert!(
        show.contains("host = \"pc.prod\"") && show.contains("readonly = true"),
        "{show}"
    );
    assert!(!show.contains("password"));

    e.ok(&["ctx", "use", "prod"]);
    assert!(e.config_text().contains("current_context = \"prod\""));
    // `show` with no NAME honours the global `-c` (which `NUTSH_CONTEXT` also feeds) rather
    // than jumping straight to current_context.
    let by_flag = e.ok(&["ctx", "show", "-c", "lab"]);
    assert!(by_flag.contains("host = \"pc.lab\""), "{by_flag}");
    assert!(
        e.fails(&["ctx", "use", "nope"], None)
            .contains("unknown context nope")
    );

    assert!(
        e.fails(
            &["ctx", "add", "prod", "--host", "x", "--username", "y"],
            None
        )
        .contains("--force")
    );
    e.ok(&[
        "ctx",
        "add",
        "prod",
        "--host",
        "pc.prod2",
        "--username",
        "svc",
        "--force",
    ]);
    assert!(e.config_text().contains("pc.prod2"));
    assert!(
        e.fails(
            &["ctx", "add", "bad name", "--host", "x", "--username", "y"],
            None
        )
        .contains("invalid context name")
    );

    e.ok(&["ctx", "remove", "prod"]);
    let text = e.config_text();
    assert!(!text.contains("[contexts.prod]"), "{text}");
    assert!(
        !text.contains("current_context"),
        "removing the current context clears it:\n{text}"
    );
    assert!(
        e.fails(&["ctx", "show", "prod"], None)
            .contains("unknown context prod")
    );
}

/// A config file with a password in it is refused - and the refusal never repeats the
/// password. The error goes to stderr and into a TUI frame, so quoting the line it sits on
/// would put the secret in both, and a half-typed password (an unterminated string) is exactly
/// the line the parser wants to quote.
#[test]
fn a_password_in_the_config_is_refused_without_being_echoed() {
    let e = Env::new();
    for text in [
        "[contexts.lab]\nhost = \"pc.lab\"\nusername = \"admin\"\npassword = \"hunter2\"\n",
        "[contexts.lab]\nhost = \"pc.lab\"\nusername = \"admin\"\npassword = \"hunter2\n",
        "password = \"hunter2\"\n[contexts.lab]\nhost = \"pc.lab\"\nusername = \"admin\"\n",
        "current_context = \"hunter2\n",
    ] {
        std::fs::write(&e.config, text).unwrap();
        let out = e.run(&["ctx", "list"], None);
        assert_eq!(out.status.code(), Some(3), "{text:?} was accepted");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("hunter2"), "{text:?} leaked: {stderr}");
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("hunter2"),
            "{text:?}"
        );
        assert!(stderr.contains("config.toml"), "{stderr}");
    }
}

#[test]
fn check_without_any_context_explains() {
    let e = Env::new();
    let err = e.fails(&["--check", "--plain-http"], Some("x"));
    assert!(err.contains("no context selected"), "{err}");
    assert!(err.contains("config.toml"), "{err}");
}

#[cfg(unix)]
#[test]
fn config_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.ok(&[
        "ctx",
        "add",
        "lab",
        "--host",
        "pc.lab",
        "--username",
        "admin",
    ]);
    assert_eq!(
        std::fs::metadata(&e.config).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

/// `--force` onto a different account must not orphan the old account's stored password: the
/// context can no longer reach it, so nothing ever would.
#[test]
fn force_repoints_context_and_drops_the_old_secret() {
    let e = Env::new();
    e.ok(&["ctx", "add", "x", "--host", "pc.x", "--username", "admin"]);
    let dir = e.state.join("nutsh").join("secrets");
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("admin@pc.x_9440");
    std::fs::write(&old, "s3cret").unwrap();
    assert_eq!(secret_column(&e.ok(&["ctx", "list"]), "x"), "file");

    e.ok(&[
        "ctx",
        "add",
        "x",
        "--host",
        "pc.x",
        "--username",
        "other",
        "--force",
    ]);
    assert!(!old.exists(), "the old account's password was orphaned");
    let list = e.ok(&["ctx", "list"]);
    assert_eq!(secret_column(&list, "x"), "none", "{list}");
}

/// The SECRET column of the row for `name`. The `*` marker on the current context is its own
/// whitespace-separated column, so the name is either the first field or the second.
fn secret_column(list: &str, name: &str) -> String {
    let row = list
        .lines()
        .skip(1)
        .find(|l| {
            let mut cols = l.split_whitespace();
            match cols.next() {
                Some("*") => cols.next() == Some(name),
                first => first == Some(name),
            }
        })
        .unwrap_or_else(|| panic!("no row for {name} in:\n{list}"));
    row.split_whitespace().last().unwrap_or("").to_string()
}

fn add_mock_context(e: &Env, pc: &MockPc, name: &str, cluster: Option<&str>) {
    let host = pc.host();
    let port = pc.port().to_string();
    let mut args = vec![
        "ctx",
        "add",
        name,
        "--host",
        &host,
        "--port",
        &port,
        "--username",
        "admin",
    ];
    if let Some(c) = cluster {
        args.extend(["--cluster", c]);
    }
    e.ok(&args);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_verifies_and_stores_then_check_uses_it() {
    let pc = MockPc::builder().start().await;
    let e = Env::new();
    add_mock_context(&e, &pc, "lab", Some("lab-cluster"));

    let err = tokio::task::block_in_place(|| {
        e.fails(&["--plain-http", "ctx", "login", "lab"], Some("wrong"))
    });
    assert!(err.contains("rejected"), "{err}");
    let secrets = e.state.join("nutsh").join("secrets");
    assert!(
        std::fs::read_dir(&secrets)
            .map(|d| d.count() == 0)
            .unwrap_or(true),
        "nothing may be stored after a failed login"
    );

    let msg = tokio::task::block_in_place(|| {
        let out = e.run(&["--plain-http", "ctx", "login", "lab"], Some("secret"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    });
    assert!(
        msg.contains("file"),
        "must say the file backend was used: {msg}"
    );
    let secret_file = secrets.join(format!("admin@{}_{}", pc.host(), pc.port()));
    assert_eq!(std::fs::read_to_string(&secret_file).unwrap(), "secret");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&secret_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let list = tokio::task::block_in_place(|| e.ok(&["ctx", "list"]));
    assert_eq!(secret_column(&list, "lab"), "file", "{list}");

    let out = tokio::task::block_in_place(|| {
        e.run(&["--plain-http", "--check", "--context", "lab"], None)
    });
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let header = stdout.lines().next().unwrap();
    assert!(header.starts_with("context lab"), "{header}");
    assert!(
        header.contains("cluster lab-cluster (0006158a-2f0d-4d5a-8e2d-000000000010)"),
        "{header}"
    );

    // The same run with no flag at all: NUTSH_CONTEXT must pick the context.
    let out = tokio::task::block_in_place(|| {
        e.cmd(&["--plain-http", "--check"])
            .env("NUTSH_CONTEXT", "lab")
            .output()
            .expect("run nutsh")
    });
    assert_eq!(
        out.status.code(),
        Some(0),
        "NUTSH_CONTEXT must select the context: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    tokio::task::block_in_place(|| e.ok(&["ctx", "remove", "lab"]));
    assert!(
        !secret_file.exists(),
        "remove must delete the stored password"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_reports_missing_and_ambiguous_clusters() {
    let pc = MockPc::builder().start().await;
    let e = Env::new();
    add_mock_context(&e, &pc, "bad", Some("nope"));
    let err = tokio::task::block_in_place(|| {
        e.fails(
            &["--plain-http", "--check", "--context", "bad"],
            Some("secret"),
        )
    });
    assert!(
        err.contains("not found") && err.contains("lab-cluster"),
        "{err}"
    );

    let amb = MockPc::builder()
        .fixtures(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/mockpc/fixtures-ambiguous"
        ))
        .start()
        .await;
    add_mock_context(&e, &amb, "amb", Some("lab-cluster"));
    let err = tokio::task::block_in_place(|| {
        e.fails(
            &["--plain-http", "--check", "--context", "amb"],
            Some("secret"),
        )
    });
    assert!(
        err.contains("ambiguous") && err.contains("000000000011"),
        "{err}"
    );
}

/// A password the store has but the Prism Central rejects looks, from the user's side, exactly
/// like a missing one; both must name the command that fixes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_password_that_stops_working_points_to_login() {
    let pc = MockPc::builder().start().await;
    let e = Env::new();
    add_mock_context(&e, &pc, "lab", None);
    let dir = e.state.join("nutsh").join("secrets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("admin@{}_{}", pc.host(), pc.port())),
        "stale",
    )
    .unwrap();
    let err = tokio::task::block_in_place(|| {
        e.fails(&["--plain-http", "--check", "--context", "lab"], None)
    });
    // "rejected" is what separates this from the there-is-no-password message, which names
    // the same command.
    assert!(
        err.contains("stored password for lab rejected")
            && err.contains("run `nutsh ctx login lab`"),
        "{err}"
    );
}

/// A 403 on the cluster list means the PC knows this account and will not let it list
/// clusters. The password is proven, so login must store it: otherwise no read-restricted
/// account could ever log in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_accepts_forbidden_as_verified() {
    let pc = MockPc::builder()
        .forbid_namespace("clustermgmt")
        .start()
        .await;
    let e = Env::new();
    add_mock_context(&e, &pc, "lab", None);
    let msg = tokio::task::block_in_place(|| {
        let out = e.run(&["--plain-http", "ctx", "login", "lab"], Some("secret"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    });
    assert!(msg.contains("not permitted"), "{msg}");
    let stored =
        e.state
            .join("nutsh")
            .join("secrets")
            .join(format!("admin@{}_{}", pc.host(), pc.port()));
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), "secret");
}

/// `-u` moves the effective account, and login must key the secret off that same account, or
/// it writes a password that connecting will never read back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_stores_under_the_username_override() {
    let pc = MockPc::builder()
        .credentials("other", "secret")
        .start()
        .await;
    let e = Env::new();
    add_mock_context(&e, &pc, "lab", None);
    tokio::task::block_in_place(|| {
        let out = e.run(
            &["--plain-http", "ctx", "login", "lab", "-u", "other"],
            Some("secret"),
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    });
    let stored =
        e.state
            .join("nutsh")
            .join("secrets")
            .join(format!("other@{}_{}", pc.host(), pc.port()));
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), "secret");

    // The context's own account was never written, and connecting under `-u other` finds the
    // key login wrote without any NUTSH_PASSWORD.
    assert!(
        !e.state
            .join("nutsh")
            .join("secrets")
            .join(format!("admin@{}_{}", pc.host(), pc.port()))
            .exists()
    );
    let out = tokio::task::block_in_place(|| {
        e.run(
            &[
                "--plain-http",
                "--check",
                "--context",
                "lab",
                "--username",
                "other",
            ],
            None,
        )
    });
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ctx login --host` has no context to key a password against; say so instead of resolving
/// to some unrelated current context.
#[test]
fn login_refuses_host_flag() {
    let e = Env::new();
    let err = e.fails(&["ctx", "login", "-H", "pc.lab", "-u", "admin"], Some("x"));
    assert!(err.contains("not --host"), "{err}");
}

/// A PC that serves older API versions: login verifies at the version clustermgmt answers,
/// and `--check` shows the step-down and the PC version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_and_check_step_down_to_the_versions_the_pc_serves() {
    let pc = MockPc::builder()
        .serve_versions("clustermgmt", &["v4.0"])
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    let e = Env::new();
    add_mock_context(&e, &pc, "old", Some("lab-cluster"));
    tokio::task::block_in_place(|| {
        let out = e.run(&["--plain-http", "ctx", "login", "old"], Some("secret"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    });
    let stdout =
        tokio::task::block_in_place(|| e.ok(&["--plain-http", "--check", "--context", "old"]));
    let header = stdout.lines().next().unwrap_or("");
    assert!(header.starts_with("context old"), "{header}");
    assert!(header.contains("pc.2024.3"), "{header}");
    assert!(header.contains("lab-cluster"), "{header}");
    let columns = |namespace: &str| -> Vec<String> {
        stdout
            .lines()
            .map(|l| l.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .find(|c| c.first().map(String::as_str) == Some(namespace))
            .unwrap_or_else(|| panic!("no row for {namespace} in:\n{stdout}"))
    };
    assert_eq!(
        columns("clustermgmt"),
        ["clustermgmt", "v4.0", "(catalog", "v4.3)", "ok"]
    );
    assert_eq!(columns("vmm"), ["vmm", "v4.1", "(catalog", "v4.3)", "ok"]);
}

/// A PC that answers at no version of clustermgmt: the password was never put to the PC, so
/// login must say it could not verify rather than store an unproven credential.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_refuses_a_pc_that_serves_no_clustermgmt() {
    let pc = MockPc::builder()
        .serve_versions("clustermgmt", &[])
        .start()
        .await;
    let e = Env::new();
    add_mock_context(&e, &pc, "x", None);
    let err = tokio::task::block_in_place(|| {
        e.fails(&["--plain-http", "ctx", "login", "x"], Some("secret"))
    });
    assert!(
        err.contains("cannot verify") && err.contains("not served at"),
        "{err}"
    );
    let secrets = e.state.join("nutsh").join("secrets");
    assert!(
        std::fs::read_dir(&secrets)
            .map(|d| d.count() == 0)
            .unwrap_or(true),
        "nothing may be stored when the password could not be verified"
    );
}

/// A `current_context` naming nothing marks no row, and a list where nothing is marked looks
/// exactly like a list where nothing was ever selected. The file still loads - that is what
/// makes it repairable - so `ctx list` is where the pointer gets named, along with the command
/// that rewrites it.
#[test]
fn list_names_a_current_context_that_points_at_nothing() {
    let e = Env::new();
    e.ok(&[
        "ctx",
        "add",
        "lab",
        "--host",
        "10.0.0.1",
        "--username",
        "admin",
    ]);
    let text = e.config_text().replace("lab\"", "gone\"");
    assert!(text.contains("current_context = \"gone\""), "{text}");
    std::fs::write(&e.config, text).unwrap();

    let out = e.ok(&["ctx", "list"]);
    assert!(out.contains("lab"), "the rows are still there: {out}");
    assert!(!out.contains('*'), "nothing is marked current: {out}");
    assert!(
        out.contains("current_context gone names no context"),
        "{out}"
    );
    assert!(out.contains("nutsh ctx use <name>"), "{out}");

    // And the advice works: the pointer is repaired without editing the file by hand.
    e.ok(&["ctx", "use", "lab"]);
    let out = e.ok(&["ctx", "list"]);
    assert!(out.contains("* lab"), "{out}");
    assert!(!out.contains("names no context"), "{out}");
}
