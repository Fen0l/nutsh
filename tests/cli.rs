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
        stdout.contains("No contexts. Press a to add one"),
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
        for args in [&["ctx", "list"][..], &["--snapshot"][..]] {
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
