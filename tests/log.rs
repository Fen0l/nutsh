//! What a log may carry, and what it may never carry.
//!
//! This program's standing rule is that a password never reaches argv, a child's environment,
//! the config file, the journal, a cache file or a fixture. A log is a new way out for all
//! three of the credentials it handles - the `Authorization` header, the session cookies Prism
//! Central issues, and a request body - so it is held to the same rule, and this is where that
//! is proved rather than asserted.
//!
//! The method is `xtask/tests/fixtures_are_clean.rs`'s: drive a real session, take everything
//! that was written down, and scan it for the values that actually went over the wire. The
//! secrets are read back off the mock rather than guessed, so the scan is looking for the exact
//! bytes this session used and not for a pattern somebody thought those bytes would match.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use nutsh_mockpc::MockPc;

/// Distinctive on purpose. A short word like `secret` occurs in a path this program prints -
/// `$XDG_STATE_HOME/nutsh/secrets` - and a scan that matched it would fail on the name of the
/// directory rather than on the contents of the file.
const PASSWORD: &str = "Tr0ub4dor-and-3-horse-battery";

fn nutsh(args: &[&str], dir: &Path, env: &[(&str, String)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nutsh"));
    cmd.args(args).stdin(Stdio::null());
    // Every `NUTSH_*` clap reads is a flag by another name, `NUTSH_LOG` now among them, so the
    // developer's own environment must not reach the child.
    for (k, _) in std::env::vars() {
        if k.starts_with("NUTSH_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("NUTSH_CONFIG", dir.join("config.toml"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("NUTSH_KEYRING", "0");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run nutsh")
}

async fn run(
    args: Vec<String>,
    dir: std::path::PathBuf,
    env: Vec<(&'static str, String)>,
) -> Output {
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        nutsh(&args, &dir, &env)
    })
    .await
    .expect("join")
}

fn config_with_lab(pc: &MockPc, dir: &Path) -> std::path::PathBuf {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "current_context = \"lab\"\n[contexts.lab]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n",
            pc.host(),
            pc.port()
        ),
    )
    .unwrap();
    let secrets = dir.join("state").join("nutsh").join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(
        secrets.join(format!("admin@{}_{}", pc.host(), pc.port())),
        PASSWORD,
    )
    .unwrap();
    dir.join("config.toml")
}

fn log_file(dir: &Path) -> std::path::PathBuf {
    dir.join("state")
        .join("nutsh")
        .join("logs")
        .join("nutsh.log")
}

/// The credential-equivalent values this session actually put on the wire, read off the mock:
/// the whole `Authorization` header and the base64 inside it, the whole `Cookie` header and
/// each session token inside that, and the password itself.
///
/// Prism Central issues `NTNX_MERCURY_IAM_SESSION`, `NTNX_MERCURY_IAM_REFRESH_TOKEN` and
/// `NTNX_IAM_SESSION`, and one of those cookies is as good as the password for about fifteen
/// minutes. The token is what has to be absent - not a truncation of it, and not a prefix.
fn secrets_on_the_wire(pc: &MockPc) -> Vec<String> {
    let mut out = vec![PASSWORD.to_string()];
    let mut presented = 0;
    let mut rode = 0;
    for request in pc.requests() {
        if let Some(auth) = request.header("authorization") {
            presented += 1;
            out.push(auth.to_string());
            if let Some(blob) = auth.split_whitespace().nth(1) {
                out.push(blob.to_string());
            }
        }
        if let Some(cookie) = request.header("cookie") {
            rode += 1;
            out.push(cookie.to_string());
            for pair in cookie.split(';') {
                if let Some((_, token)) = pair.split_once('=') {
                    out.push(token.trim().to_string());
                }
            }
        }
    }
    assert!(presented > 0, "no request presented the credential");
    assert!(rode > 0, "no request rode a session cookie");
    out.sort();
    out.dedup();
    out
}

/// Every line, with the crate that wrote it. The `fmt` layer draws
/// `TIMESTAMP LEVEL target: message fields`, so the third field is the target.
fn targets(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .map(|t| t.trim_end_matches(':'))
        .collect()
}

fn assert_clean(what: &str, text: &str, secrets: &[String]) {
    assert!(!text.is_empty(), "{what} is empty; nothing was logged");
    assert!(
        text.contains("nutsh starting"),
        "{what} has no banner, so no subscriber was installed:\n{text}"
    );
    let found: Vec<&String> = secrets
        .iter()
        .filter(|s| text.contains(s.as_str()))
        .collect();
    assert!(
        found.is_empty(),
        "{what} carries {} credential value(s) that went over the wire: {found:?}",
        found.len()
    );
    // The other half of the rule. `hyper` and `h2` log the frames a request is made of, and an
    // HPACK frame is a header dump with the credential still in it, so no level and no filter
    // string may let a crate that is not ours reach the writer.
    let theirs: Vec<&str> = targets(text)
        .into_iter()
        .filter(|t| !t.starts_with("nutsh"))
        .collect();
    assert!(theirs.is_empty(), "{what} carries lines from {theirs:?}");
}

/// The CLI case: `--check` has a terminal it is already writing to, so the lines go to stderr
/// and no file is left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cli_run_logs_to_stderr_and_leaks_nothing() {
    let pc = MockPc::builder()
        .credentials("admin", PASSWORD)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        vec![
            "--check".into(),
            "--plain-http".into(),
            "--host".into(),
            pc.host(),
            "--port".into(),
            pc.port().to_string(),
            "--username".into(),
            "admin".into(),
        ],
        dir.path().to_path_buf(),
        vec![
            ("NUTSH_PASSWORD", PASSWORD.into()),
            ("NUTSH_LOG", "trace".into()),
        ],
    )
    .await;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_clean("the stderr of --check", &stderr, &secrets_on_the_wire(&pc));
    assert!(
        !log_file(dir.path()).exists(),
        "a run that can use stderr writes no file"
    );
    // Every request, through the one funnel, with what a person reading this log is looking
    // for: what was asked, of what, what came back, how long it took, and what it carried.
    let requests: Vec<&str> = stderr.lines().filter(|l| l.contains("request")).collect();
    assert!(
        requests.len() >= pc.requests().len(),
        "{} requests reached the mock and {} were logged",
        pc.requests().len(),
        requests.len()
    );
    let one = requests
        .iter()
        .find(|l| l.contains("status=200"))
        .unwrap_or_else(|| panic!("no answered request:\n{stderr}"));
    for field in [
        "method=GET",
        "url=http://",
        "status=200",
        "ms=",
        "credential=",
    ] {
        assert!(one.contains(field), "no {field} in {one}");
    }
    // Both halves of the session arithmetic are visible: the request that presented the
    // credential, and the ones that rode what it opened.
    assert!(
        requests.iter().any(|l| l.contains("credential=presented")),
        "{stderr}"
    );
    assert!(
        requests.iter().any(|l| l.contains("credential=session")),
        "{stderr}"
    );
}

/// The question this was built to answer: a stored credential is refused after several good
/// runs, and nothing says which call it was.
///
/// It says now, and it says which of the two things a 401 means. A 401 on a request that rode a
/// session is that session having ended; a 401 on the request that then **presented** the
/// credential is the credential being refused, terminal and never retried. That asymmetry is
/// what `Client::send` is built around, and until now it was invisible from outside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_credential_names_the_request_it_was_refused_on() {
    let pc = MockPc::builder()
        .credentials("admin", PASSWORD)
        // The password is right and one namespace answers 401 anyway: the account may not read
        // it, and the session everything else rides is fine.
        .fail_namespace("volumes", 401)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        vec![
            "--check".into(),
            "--plain-http".into(),
            "--host".into(),
            pc.host(),
            "--port".into(),
            pc.port().to_string(),
            "--username".into(),
            "admin".into(),
        ],
        dir.path().to_path_buf(),
        vec![
            ("NUTSH_PASSWORD", PASSWORD.into()),
            ("NUTSH_LOG", "debug".into()),
        ],
    )
    .await;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let refused: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("status=401"))
        .collect();
    // One per version the negotiation tried, every one on the session, and no renewal after
    // any of them: the session still answered elsewhere, so each 401 was that endpoint's.
    assert!(!refused.is_empty(), "{stderr}");
    for line in &refused {
        // Not a `debug` detail. Somebody who turned logging on because a session stopped
        // working should not have to raise the level to find the reason.
        assert!(line.contains("WARN"), "{line}");
        assert!(line.contains("method=GET"), "{line}");
        assert!(line.contains("/api/volumes/"), "the request it was: {line}");
        assert!(
            line.contains("credential=session"),
            "it rode the session, and no password followed: {line}"
        );
    }
    assert!(
        !stderr.contains("credential=refused"),
        "nothing was refused before the wire afterwards:\n{stderr}"
    );
    assert!(
        !stderr.contains(PASSWORD),
        "a refused password is still a password:\n{stderr}"
    );
}

/// The TUI case, of which `--snapshot` is one: the alternate screen and a frame on stdout both
/// mean stderr is not available, so the lines go to a file under `$XDG_STATE_HOME/nutsh`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_logs_to_a_file_and_leaks_nothing() {
    let pc = MockPc::builder()
        .credentials("admin", PASSWORD)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    config_with_lab(&pc, dir.path());
    let out = run(
        vec!["vm".into(), "--snapshot".into(), "--plain-http".into()],
        dir.path().to_path_buf(),
        vec![("NUTSH_LOG", "trace".into())],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("NAME"), "no frame:\n{stdout}");
    // The reason there is a file at all: a log line on stderr is a hole in the frame.
    assert!(
        !stderr.contains("nutsh starting"),
        "the frame's stderr carries log lines:\n{stderr}"
    );
    let path = log_file(dir.path());
    let written =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_clean("the log file", &written, &secrets_on_the_wire(&pc));
}

/// `NUTSH_LOG` takes a `tracing_subscriber` filter string as well as a level word, and a filter
/// string is a way to ask for somebody else's crate by name. `h2` logs the frames a request is
/// made of, and an HPACK frame is a header dump with the credential still in it, so the answer
/// to asking is nothing at all: the writer takes this program's own crates and no others,
/// whatever the filter says.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_string_that_asks_for_everything_still_gets_only_us() {
    let pc = MockPc::builder()
        .credentials("admin", PASSWORD)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    config_with_lab(&pc, dir.path());
    let out = run(
        vec!["vm".into(), "--snapshot".into(), "--plain-http".into()],
        dir.path().to_path_buf(),
        // A bare level is a global default for every crate in the tree, and the two named after
        // it are the ones that would carry the credential if anything could.
        vec![("NUTSH_LOG", "trace,h2=trace,hyper=trace".into())],
    )
    .await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read_to_string(log_file(dir.path())).expect("a filter that asked");
    assert_clean(
        "the log a filter string asked for",
        &written,
        &secrets_on_the_wire(&pc),
    );
}

/// The default, which is the promise the whole thing rests on: a user who asks for nothing gets
/// the behaviour this program had before it could log, and no file on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_nobody_asked_anything_of_writes_nothing() {
    let pc = MockPc::builder()
        .credentials("admin", PASSWORD)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    config_with_lab(&pc, dir.path());
    let out = run(
        vec!["vm".into(), "--snapshot".into(), "--plain-http".into()],
        dir.path().to_path_buf(),
        vec![],
    )
    .await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !log_file(dir.path()).exists(),
        "a run at the default level left a file behind"
    );
    assert!(
        !dir.path().join("state").join("nutsh").join("logs").exists(),
        "a run at the default level made the directory"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "and it said nothing on stderr"
    );
}

/// The config file says it when the environment does not, and the environment wins when both
/// do. `off` in the environment over a file that asked for a log is a run with no log, which is
/// the half of the precedence that a user reaches for when they want quiet back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_file_asks_for_a_log_and_the_environment_overrules_it() {
    let pc = MockPc::builder()
        .credentials("admin", PASSWORD)
        .start()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_lab(&pc, dir.path());
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("log = \"debug\"\n{text}")).unwrap();

    let args = vec!["vm".into(), "--snapshot".into(), "--plain-http".into()];
    let out = run(args.clone(), dir.path().to_path_buf(), vec![]).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read_to_string(log_file(dir.path())).expect("the file asked for it");
    assert!(written.contains("nutsh starting"), "{written}");
    assert_clean(
        "the log the file asked for",
        &written,
        &secrets_on_the_wire(&pc),
    );

    std::fs::remove_file(log_file(dir.path())).unwrap();
    let out = run(
        args,
        dir.path().to_path_buf(),
        vec![("NUTSH_LOG", "off".into())],
    )
    .await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !log_file(dir.path()).exists(),
        "NUTSH_LOG=off over a file that asked for a log wrote one anyway"
    );
}
