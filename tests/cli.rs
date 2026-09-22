use std::process::Command;

/// A child that inherits none of this shell's `NUTSH_*` variables: every one of them is a
/// flag by another name, so `NUTSH_PORT=1 cargo test` must not change what these tests run.
fn nutsh() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nutsh"));
    for (k, _) in std::env::vars() {
        if k.starts_with("NUTSH_") {
            cmd.env_remove(k);
        }
    }
    cmd
}

#[test]
fn version_prints_name_and_version() {
    let out = nutsh().arg("--version").output().expect("run nutsh");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("nutsh {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn info_prints_build_target() {
    let out = nutsh().arg("--info").output().expect("run nutsh");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("nutsh "), "{stdout}");
    assert!(stdout.contains("target: "), "{stdout}");
    assert!(stdout.contains("reach: "), "{stdout}");
    assert!(stdout.contains(" direct, "), "{stdout}");
}

/// Nothing to connect to is not a failure: the Contexts screen is the answer, and it names
/// the config file it read nothing out of.
#[test]
fn without_a_config_the_snapshot_is_the_empty_contexts_screen() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("absent.toml");
    let out = nutsh()
        .arg("--snapshot")
        .env("NUTSH_CONFIG", &config)
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("NUTSH_KEYRING", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run nutsh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.starts_with("╭ nutsh "), "{stdout}");
    assert!(stdout.contains("Context:    no session"), "{stdout}");
    assert!(stdout.contains("Count:      0 contexts"), "{stdout}");
    // The file name, not the whole path: `Config:` elides a long temp path from the left.
    assert!(
        stdout.contains("Config:") && stdout.contains("absent.toml"),
        "{stdout}"
    );
    assert!(
        stdout.contains("No contexts. Press a to add one, run nutsh ctx add, or try nutsh demo."),
        "{stdout}"
    );
}

/// `--readonly` is this run's flag, not a property to be written down. `ctx add --readonly` is
/// a different flag of the same name, and the two must not be one: a session someone wanted
/// read-only once should never leave the context read-only for good.
#[test]
fn readonly_before_a_subcommand_is_not_ctx_adds_readonly() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let out = nutsh()
        .args([
            "--readonly",
            "ctx",
            "add",
            "lab",
            "--host",
            "10.0.0.1",
            "--username",
            "admin",
        ])
        .env("NUTSH_CONFIG", &config)
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("NUTSH_KEYRING", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run nutsh");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&config).expect("the context was written");
    assert!(text.contains("[contexts.lab]"), "{text}");
    assert!(!text.contains("readonly = true"), "{text}");
}

/// `--size` is a terminal, not an arbitrary allocation: a zero-sized frame is not a frame, and
/// a typo asking for millions of cells must be refused by clap rather than aborting the
/// process when the buffer is allocated.
#[test]
fn bad_sizes_are_refused_by_clap() {
    for bad in ["0x0", "12", "2000x10", "10x0", "-1x10", "120x40x2"] {
        let out = nutsh()
            .args(["--snapshot", "--size", bad])
            .output()
            .expect("run nutsh");
        assert_eq!(out.status.code(), Some(2), "{bad} was accepted");
    }
    // The message names the shape and the range, so the fix is readable off the refusal.
    let out = nutsh()
        .args(["--snapshot", "--size", "2000x10"])
        .output()
        .expect("run nutsh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("columns must be a number from 1 to 1000"),
        "{stderr}"
    );
}

/// A TUI argument and a subcommand - or `--check` / `--info` - in one command line is a
/// mistake with two readings, and running one of them while dropping the other is the worst
/// of them.
#[test]
fn a_kind_cannot_be_given_alongside_a_subcommand_or_check() {
    for args in [
        vec!["vm", "ctx", "list"],
        vec!["vm", "--check"],
        vec!["vm", "--info"],
        vec!["--snapshot", "ctx", "list"],
        vec!["--size", "80x20", "ctx", "list"],
        vec!["vm", "demo"],
        vec!["--snapshot", "demo"],
        vec!["--snapshot", "--check"],
        vec!["--size", "80x20", "--info"],
    ] {
        let out = nutsh().args(&args).output().expect("run nutsh");
        assert_eq!(out.status.code(), Some(2), "{args:?} was accepted");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("cannot be used with"), "{args:?}: {stderr}");
    }
}

/// A `[[guardrails]]` file that will not load stops every command path before it does anything,
/// naming the file and the value. A safety rule that silently did nothing would be worse than
/// none, and the TUI's session builder falls back to defaults for a file it cannot read, so the
/// refusal has to happen here, ahead of it, for `ctx list` and the TUI alike.
#[test]
fn a_broken_guardrail_file_stops_every_command_path_naming_the_value() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    for (text, wants) in [
        (
            "[[guardrails]]\nconfirm = \"maybe\"\n",
            &["maybe", "type-name"][..],
        ),
        ("[[guardrails]]\nconfrim = \"yes\"\n", &["confrim"][..]),
    ] {
        std::fs::write(&config, text).unwrap();
        for args in [
            &["ctx", "list"][..],
            &["--snapshot"][..],
            &["demo", "--snapshot"][..],
        ] {
            let out = nutsh()
                .args(args)
                .env("NUTSH_CONFIG", &config)
                .env("XDG_STATE_HOME", dir.path().join("state"))
                .env("NUTSH_KEYRING", "0")
                .stdin(std::process::Stdio::null())
                .output()
                .expect("run nutsh");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.code(),
                Some(3),
                "{args:?} with {text:?}: {stderr}"
            );
            assert!(
                stderr.contains(config.to_str().unwrap()),
                "{args:?}: {stderr}"
            );
            for want in wants {
                assert!(stderr.contains(want), "{args:?}: {stderr}");
            }
        }
    }
}

/// `cache clear` is the command that empties the cache, and it must not quietly stop being
/// that on a machine with `NUTSH_CONTEXT` exported - which is the normal state for this tool.
/// The narrowing flag is `--only`, with a clap id of its own, precisely because clap copies a
/// global's value into every subcommand's matches under the global's id.
#[test]
fn cache_clear_ignores_the_exported_context_and_narrows_only_for_only() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let root = state.join("nutsh").join("cache");
    let seed = || {
        for name in ["lab", "other"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("meta.json"), "{}").unwrap();
        }
    };
    let clear = |args: &[&str]| {
        let out = nutsh()
            .arg("cache")
            .arg("clear")
            .args(args)
            .env("NUTSH_CONFIG", dir.path().join("absent.toml"))
            .env("XDG_STATE_HOME", &state)
            .env("NUTSH_KEYRING", "0")
            // Exported by the shell this is meant to survive.
            .env("NUTSH_CONTEXT", "lab")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run nutsh");
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    seed();
    let stdout = clear(&["--only", "lab"]);
    assert!(stdout.contains("removed "), "{stdout}");
    assert!(!root.join("lab").exists(), "{stdout}");
    assert!(root.join("other").exists(), "only the named one: {stdout}");

    seed();
    let stdout = clear(&[]);
    assert!(!root.exists(), "the whole tree, exported variable and all");
    assert!(stdout.contains("removed "), "{stdout}");

    // Twice is not an error: what was asked for is already true.
    let stdout = clear(&[]);
    assert!(stdout.contains("nothing cached under"), "{stdout}");
}

/// `cache info` connects to nothing and says so plainly when there is nothing to say.
#[test]
fn cache_info_on_an_empty_state_directory_says_nothing_is_cached() {
    let dir = tempfile::tempdir().unwrap();
    let out = nutsh()
        .args(["cache", "info"])
        .env("NUTSH_CONFIG", dir.path().join("absent.toml"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("NUTSH_KEYRING", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run nutsh");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("nothing cached under "), "{stdout}");
    assert!(stdout.trim_end().ends_with("nutsh/cache"), "{stdout}");
}

/// `completions <shell>` prints the script that wires the shell back to this binary; a shell
/// it does not know is refused with the ones it does.
#[test]
fn completions_prints_a_script_for_a_known_shell_and_names_the_rest() {
    for shell in ["zsh", "bash", "fish"] {
        let out = nutsh()
            .args(["completions", shell])
            .output()
            .expect("run nutsh");
        assert_eq!(out.status.code(), Some(0), "{shell}");
        let script = String::from_utf8_lossy(&out.stdout);
        assert!(
            script.contains("COMPLETE=") && script.contains("nutsh"),
            "{shell}: {script}"
        );
    }
    let out = nutsh()
        .args(["completions", "csh"])
        .output()
        .expect("run nutsh");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("zsh") && stderr.contains("fish"),
        "{stderr}"
    );
}

/// The dynamic half, asked the way the fish script asks it: `[KIND]` completes from the
/// catalog - ids, aliases and pages - and `-c` from the context names in the config file, and
/// nothing in what comes back came from anywhere but those two.
#[test]
fn completion_offers_kinds_pages_and_the_config_files_contexts() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "current_context = \"lab\"\n[contexts.lab]\nhost = \"pc.example.test\"\nport = 9440\nusername = \"admin\"\n[contexts.prod]\nhost = \"pc2.example.test\"\nport = 9440\nusername = \"ops\"\n",
    )
    .unwrap();
    let complete = |words: &[&str]| {
        let mut cmd = nutsh();
        cmd.env("COMPLETE", "fish")
            .env("NUTSH_CONFIG", &config)
            .env("NUTSH_KEYRING", "0")
            .env("NUTSH_PASSWORD", "hunter2")
            .arg("--")
            .arg("nutsh")
            .args(words);
        let out = cmd.output().expect("run nutsh");
        assert_eq!(out.status.code(), Some(0), "{words:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let kinds = complete(&["v"]);
    let values: Vec<&str> = kinds.lines().filter_map(|l| l.split('\t').next()).collect();
    assert!(values.contains(&"vm"), "an alias: {kinds}");
    assert!(values.contains(&"vmm.ahv.config.Vm"), "an id: {kinds}");
    assert!(
        kinds
            .lines()
            .any(|l| l.starts_with("vm\t") && l.contains("Virtual Machines")),
        "what the alias opens rides beside it: {kinds}"
    );
    let pages = complete(&["dash"]);
    assert!(
        pages.lines().any(|l| l.starts_with("dashboard\t")),
        "a page: {pages}"
    );

    let contexts = complete(&["-c", ""]);
    let names: Vec<&str> = contexts
        .lines()
        .filter_map(|l| l.split('\t').next())
        .collect();
    assert_eq!(names, ["lab", "prod"], "{contexts}");
    assert!(
        !contexts.contains("hunter2"),
        "never the password: {contexts}"
    );
    let only = complete(&["cache", "clear", "--only", ""]);
    assert!(only.lines().any(|l| l.starts_with("prod\t")), "{only}");
}

/// `nutsh demo …` the way a first run meets it: no config file, no state directory, a
/// `NUTSH_HOST` the shell happens to export, nothing to type into. The scratch directory the
/// demo writes its context file in is checked gone on the way out, whatever the exit was.
fn demo(args: &[&str], dir: &std::path::Path) -> std::process::Output {
    let child = nutsh()
        .arg("demo")
        .args(args)
        .env("NUTSH_CONFIG", dir.join("absent.toml"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("NUTSH_KEYRING", "0")
        .env("NUTSH_HOST", "203.0.113.9")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run nutsh");
    let scratch = std::env::temp_dir().join(format!("nutsh-demo-{}", child.id()));
    let out = child.wait_with_output().expect("run nutsh");
    assert!(!scratch.exists(), "{} outlived the demo", scratch.display());
    out
}

/// A demo run's frame, once the run is known to have exited 0.
fn stdout_of(out: &std::process::Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The whole point of the subcommand: a person with no Prism Central, no config and no
/// credentials gets a populated frame, and afterwards has exactly what they had before - no
/// config file, no state directory. The exported `NUTSH_HOST` is parsed and never read.
#[test]
fn demo_snapshot_needs_no_config_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out = demo(&["vm", "--snapshot"], dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Context:    demo"), "{stdout}");
    assert!(!stdout.contains("+demo-dr"), "nothing joined: {stdout}");
    assert!(stdout.contains("pc.7.6"), "{stdout}");
    assert!(stdout.contains("PC:         127.0.0.1:"), "{stdout}");
    // With the colon: the estate's own addresses live in `203.0.113.0/24` too, and a bare
    // `203.0.113.9` would match a VM at `.9x`; `host:port` is the shape the header prints.
    assert!(!stdout.contains("203.0.113.9:"), "{stdout}");
    for name in ["prd-web-01", "prd-db-01", "dev-scratch-02"] {
        assert!(stdout.contains(name), "{name}: {stdout}");
    }
    assert_eq!(stdout.lines().count(), 40, "default size is 120x40");
    assert!(
        !dir.path().join("absent.toml").exists(),
        "the demo wrote a config file"
    );
    assert!(
        !dir.path().join("state").exists(),
        "the demo wrote under the state directory"
    );

    // The table instead of the frame, through the subcommand's own `--format`.
    let out = demo(&["vm", "--snapshot", "--format", "csv"], dir.path());
    let csv = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{csv}");
    assert!(
        csv.lines().next().is_some_and(|h| h.contains("NAME")),
        "{csv}"
    );
    assert!(csv.contains("prd-web-01"), "{csv}");
    assert!(!dir.path().join("state").exists());
}

/// `--readonly` is the demo's own flag, the same shape as `ctx add`'s: the badge is on the
/// frame, and the top-level one before the subcommand is accepted and ignored like it is for
/// every subcommand.
#[test]
fn demo_readonly_shows_the_badge() {
    let dir = tempfile::tempdir().unwrap();
    let out = demo(&["vm", "--snapshot", "--readonly"], dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("[read-only]"), "{stdout}");

    let out = demo(&["vm", "--snapshot"], dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("[read-only]"), "{stdout}");

    let out = nutsh()
        .args(["--readonly", "demo", "vm", "--snapshot"])
        .env("NUTSH_CONFIG", dir.path().join("absent.toml"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("NUTSH_KEYRING", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run nutsh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        !stdout.contains("[read-only]"),
        "the top-level flag is ignored beside a subcommand: {stdout}"
    );
}

/// `nutsh demo --help` says what the subcommand will not read, so the person with `NUTSH_HOST`
/// exported is told rather than surprised.
#[test]
fn demo_help_says_what_it_ignores() {
    let out = nutsh()
        .args(["demo", "--help"])
        .output()
        .expect("run nutsh");
    assert_eq!(out.status.code(), Some(0));
    let help = String::from_utf8_lossy(&out.stdout);
    for want in [
        "NUTSH_* variables are ignored",
        "--snapshot",
        "--size <COLSxROWS>",
        "--format <text|csv|json>",
        "--readonly",
        "--join",
        "--site <NAME>",
        "[KIND]",
    ] {
        assert!(help.contains(want), "{want}: {help}");
    }
}

/// The subcommand is offered where the other three are, and its `[KIND]` completes from the
/// catalog like the top-level one.
#[test]
fn completions_list_demo() {
    let dir = tempfile::tempdir().unwrap();
    let complete = |words: &[&str]| {
        let mut cmd = nutsh();
        cmd.env("COMPLETE", "fish")
            .env("NUTSH_CONFIG", dir.path().join("absent.toml"))
            .env("NUTSH_KEYRING", "0")
            .arg("--")
            .arg("nutsh")
            .args(words);
        let out = cmd.output().expect("run nutsh");
        assert_eq!(out.status.code(), Some(0), "{words:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let subs = complete(&["de"]);
    assert!(
        subs.lines().any(|l| l.split('\t').next() == Some("demo")),
        "{subs}"
    );
    let kinds = complete(&["demo", "v"]);
    let values: Vec<&str> = kinds.lines().filter_map(|l| l.split('\t').next()).collect();
    assert!(values.contains(&"vm"), "{kinds}");
    assert!(values.contains(&"vmm.ahv.config.Vm"), "{kinds}");
}

/// `--join`: both Prism Centrals in one table from the first frame, each row saying where it
/// is from, the header naming both, the count summed (12 + 5).
#[test]
fn demo_join_merges_both_sites() {
    let dir = tempfile::tempdir().unwrap();
    let out = demo(
        &["vm", "--join", "--snapshot", "--size", "140x40"],
        dir.path(),
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Context:    demo +demo-dr"), "{stdout}");
    assert!(stdout.contains("CONTEXT"), "{stdout}");
    assert!(stdout.contains("Virtual Machines [17]"), "{stdout}");
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("prd-web-01") && l.contains("demo"))
            && stdout
                .lines()
                .any(|l| l.contains("dr-web-01") && l.contains("demo-dr")),
        "{stdout}"
    );
}

/// `--site demo-dr`: the second Prism Central is the session, under its own account, with its
/// own rows and its own counters.
#[test]
fn demo_site_dr_starts_on_the_second_pc() {
    let dir = tempfile::tempdir().unwrap();
    let out = demo(
        &["vm", "--site", "demo-dr", "--snapshot", "--size", "120x30"],
        dir.path(),
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Context:    demo-dr"), "{stdout}");
    assert!(!stdout.contains("+demo"), "not joined: {stdout}");
    assert!(
        stdout.contains("dr-web-01") && stdout.contains("ridge-jump-01"),
        "{stdout}"
    );
    assert!(!stdout.contains("prd-web-01"), "{stdout}");
    assert!(stdout.contains("clusters 1"), "{stdout}");
    assert!(stdout.contains("vms 5"), "{stdout}");
    assert!(stdout.contains("pc.7.6"), "{stdout}");
}

/// `--site demo-dr --join` joins the harbor site, since a session cannot join itself.
#[test]
fn demo_site_dr_with_join_reads_harbor_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let out = demo(
        &[
            "vm",
            "--site",
            "demo-dr",
            "--join",
            "--snapshot",
            "--size",
            "140x40",
        ],
        dir.path(),
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Context:    demo-dr +demo"), "{stdout}");
    assert!(stdout.contains("Virtual Machines [17]"), "{stdout}");
}

/// The Disaster Recovery page on the harbor site names the remote site: `pc-ridge` in the
/// SITES and RECOVERY SITE cells, never the eight-hex stub of its domain manager. The sampler
/// walks all twelve VMs before the frame is printed.
#[test]
fn demo_dr_snapshot_names_the_remote_site() {
    let dir = tempfile::tempdir().unwrap();
    let out = demo(
        &["disaster-recovery", "--snapshot", "--size", "140x44"],
        dir.path(),
    );
    let stdout = stdout_of(&out);
    // The summary column right-aligns its number: `│Sampled VMs           12│`.
    let sampled = stdout
        .lines()
        .find(|l| l.contains("Sampled VMs"))
        .unwrap_or_else(|| panic!("no summary line: {stdout}"));
    let number = sampled
        .split("Sampled VMs")
        .nth(1)
        .unwrap()
        .trim_matches(|c: char| c == ' ' || c == '│');
    assert_eq!(number, "12", "{sampled}");
    assert!(stdout.contains("pc-ridge"), "{stdout}");
    let ridge_dm: String = {
        let (_, text) = nutsh_mockpc::demo::DEMO
            .iter()
            .find(|(p, _)| p.ends_with("registered-domains.json"))
            .expect("harbor registers pc-ridge");
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        v[0]["extId"].as_str().unwrap().to_string()
    };
    assert!(
        !stdout.contains(&ridge_dm[..8]),
        "the remote site is a name, not a stub: {stdout}"
    );
}

/// `--site` completes to the three built-in names and nothing else.
#[test]
fn completion_offers_the_demo_sites() {
    let mut cmd = nutsh();
    cmd.env("COMPLETE", "fish")
        .env("NUTSH_KEYRING", "0")
        .args(["--", "nutsh", "demo", "--site", ""]);
    let out = cmd.output().expect("run nutsh");
    assert_eq!(out.status.code(), Some(0));
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split('\t').next().map(str::to_string))
        .collect();
    assert_eq!(names, ["demo", "demo-dr", "demo-edge"], "{names:?}");
}
