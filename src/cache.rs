//! `nutsh cache info` and `nutsh cache clear`. Neither connects to anything, and between them
//! they answer every "is it the cache?" question.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use nutsh_core::cache::{self, Peeked};
use nutsh_prism::Profile;

/// The cache directory a session would use and whatever is in it, read **before** anything
/// connects: a few milliseconds of local I/O ahead of a 1.5-to-4-second connect is what lets
/// the first frame paint rows and lets `connect` adopt pins instead of negotiating.
///
/// The key is the context name when the session has one, else `username@host:port`, always
/// through [`cache::slug`]. `Err(name)` is a name `slug` refuses - `.`, `..`, blank - and that
/// session runs cacheless: nothing read, nothing written.
///
/// One function for two callers, because a second spelling of the key would give one context
/// two directories: `crate::tui` reads it before the command line's connect, and
/// `crate::contexts` before a `:ctx` switch's.
pub(crate) fn open_for(
    context: Option<&str>,
    profile: &Profile,
    max_age: u64,
) -> Result<(PathBuf, Option<cache::Restored>), String> {
    let name = context.map_or_else(
        || format!("{}@{}:{}", profile.username, profile.host, profile.port),
        str::to_string,
    );
    let Some(key) = cache::slug(&name) else {
        return Err(name);
    };
    let dir = nutsh_config::cache_dir().join(key);
    // The half of the identity known before a single request. The domain manager and the PC
    // version are compared by `session::connect`, once they have answered.
    let identity = cache::Identity {
        host: profile.host.clone(),
        port: profile.port,
        username: profile.username.clone(),
        domain_manager: None,
        pc_version: None,
    };
    let restored = cache::read(&dir, &identity, cache::now_secs(), max_age);
    Ok((dir, restored))
}

#[derive(Subcommand, Debug)]
pub(crate) enum CacheCommand {
    /// Print what is cached, per context
    Info,
    /// Remove one context's cache, or the whole tree
    Clear {
        /// Only this context's directory; without it the whole tree goes
        ///
        /// This flag narrows the delete, and the global `-c` / `--context` does not: that one
        /// names a Prism Central to connect to and is read from `NUTSH_CONTEXT`, and a delete
        /// that narrowed itself to whatever a shell exported could never empty the cache.
        //
        // Hence the id of its own, too: clap copies a global's value into every subcommand's
        // matches under the global's id, so a field named `context` here would have been
        // filled in by `NUTSH_CONTEXT` with nothing on screen to say so. Pinned by
        // `the_global_context_never_narrows_a_delete`.
        #[arg(id = "clear_only", long = "only", value_name = "CONTEXT", add = clap_complete::ArgValueCandidates::new(crate::completions::contexts))]
        context: Option<String>,
    },
}

pub(crate) fn run(action: CacheCommand) -> anyhow::Result<i32> {
    let root = nutsh_config::cache_dir();
    match action {
        CacheCommand::Info => info(&root),
        CacheCommand::Clear { context } => clear(&root, context.as_deref()),
    }
}

/// The directory `--only` names, `None` for the whole tree, or an error for a name that is
/// not a usable directory key. **The same `slug` the session uses**: this is the one command
/// that deletes, and it is where an unslugged `..` would have deleted the parent directory.
fn target_dir(root: &Path, context: Option<&str>) -> anyhow::Result<Option<PathBuf>> {
    match context {
        None => Ok(None),
        Some(name) => match cache::slug(name) {
            Some(key) => Ok(Some(root.join(key))),
            None => anyhow::bail!("{name:?} is not a usable context name for a cache directory"),
        },
    }
}

fn clear(root: &Path, context: Option<&str>) -> anyhow::Result<i32> {
    // The name and its directory are bound as a pair: `target_dir` yields a directory exactly
    // when a name was given, so matching both here spares the message a fallback string that
    // could never print and a reader the work of proving it never does.
    let (dir, what) = match (context, target_dir(root, context)?) {
        (Some(name), Some(dir)) => (dir, format!("for {name}")),
        _ => (root.to_path_buf(), format!("under {}", root.display())),
    };
    if !dir.exists() {
        println!("nothing cached {what}");
        return Ok(0);
    }
    let bytes = dir_bytes(&dir);
    // `cache::remove` for the tree as well as for one context: it is the one that names the
    // path in the error, so a read-only file system says which directory it refused.
    cache::remove(&dir)?;
    println!("removed {} ({bytes} bytes)", dir.display());
    Ok(0)
}

fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        // `entry.file_type()` rather than `path.is_dir()`, and `entry.metadata()` rather than
        // `std::fs::metadata`: neither follows a symlink, and neither does `remove_dir_all`.
        // A link planted in the cache would otherwise be counted as the tree it points at, and
        // the printed figure is meant to be what was removed.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            total += dir_bytes(&entry.path());
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

fn info(root: &Path) -> anyhow::Result<i32> {
    let now = cache::now_secs();
    let Ok(entries) = std::fs::read_dir(root) else {
        println!("nothing cached under {}", root.display());
        return Ok(0);
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    if dirs.is_empty() {
        println!("nothing cached under {}", root.display());
        return Ok(0);
    }
    println!("{}", root.display());
    for dir in dirs {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Reading the meta directly rather than through `cache::read`: `info` reports what is
        // there, including a directory this build would refuse to use, and must never be the
        // command that deletes one.
        match cache::peek(&dir) {
            Some(peeked) => {
                print!("{}", report(&name, &peeked, now));
                print!("{}", history_line(&dir).unwrap_or_default());
            }
            None => println!("  {name}: unreadable"),
        }
    }
    Ok(0)
}

/// One context's history line, or nothing when it has no history file or this build cannot
/// read it. `clear` needs no change at all: it removes the directory, and the history is inside
/// it - which is what "removes every byte written" has always promised.
fn history_line(dir: &Path) -> Option<String> {
    let n = nutsh_core::history::count(dir)?;
    Some(format!(
        "    history: {n} {}\n",
        if n == 1 { "line" } else { "lines" }
    ))
}

/// One context's line, plus one per table. A [`Peeked`] and not its pieces: `report` prints
/// everything the meta said, the refusal included, and a caller that had to pick the fields
/// would be the place a new one got forgotten.
fn report(name: &str, peeked: &Peeked, now: u64) -> String {
    let id = &peeked.identity;
    let version = id
        .pc_version
        .as_ref()
        .map(|v| format!("  {v}"))
        .unwrap_or_default();
    let pins = peeked.pins;
    let pin_word = if pins == 1 { "pin" } else { "pins" };
    let mut out = format!(
        "  {name}: {}@{}:{}{version}  {}  {pins} {pin_word}{}\n",
        id.username,
        id.host,
        id.port,
        age(now.saturating_sub(peeked.written)),
        refusal(peeked),
    );
    for t in &peeked.tables {
        out.push_str(&format!(
            "    {}  {} rows  {} bytes\n",
            t.kind, t.rows, t.bytes
        ));
    }
    out
}

/// Why this build would not use the directory, or nothing when it would. `cache::read`
/// discards on the format and on the app version - and the app version changes with every
/// release - so a directory that is plainly there can still be dead. Saying so is the whole
/// point of `info`: "why was the first frame empty?" has to be answerable without a debugger.
fn refusal(peeked: &Peeked) -> String {
    let mut why = Vec::new();
    if peeked.format != cache::FORMAT {
        why.push(format!("format {}", peeked.format));
    }
    if peeked.app_version != env!("CARGO_PKG_VERSION") {
        why.push(format!("nutsh {}", peeked.app_version));
    }
    if why.is_empty() {
        String::new()
    } else {
        format!("  ({} - this build will not use it)", why.join(", "))
    }
}

/// Unix seconds as a person reads them. Two units at most: past a day, minutes are noise.
fn age(secs: u64) -> String {
    match secs {
        0 => "just now".to_string(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3_600 => format!("{}m{}s ago", s / 60, s % 60),
        s if s < 86_400 => format!("{}h{}m ago", s / 3_600, (s % 3_600) / 60),
        s => format!("{}d{}h ago", s / 86_400, (s % 86_400) / 3_600),
    }
}

#[cfg(test)]
mod tests {
    use nutsh_core::cache::{FORMAT, Identity, TableRecord};

    use super::*;

    fn peeked(format: u32, app_version: &str, pins: usize) -> Peeked {
        Peeked {
            format,
            app_version: app_version.to_string(),
            written: 1_000,
            identity: Identity {
                host: "pc.lab.example".into(),
                port: 9440,
                username: "admin".into(),
                domain_manager: Some("dm-1".into()),
                pc_version: Some("pc.7.6".into()),
            },
            pins,
            tables: vec![TableRecord {
                file: "t-0123456789abcdef.json".into(),
                kind: "vmm.ahv.config.Vm".into(),
                parents: Vec::new(),
                filter: None,
                select: None,
                rows: 100,
                total: Some(100),
                bytes: 749_547,
                at: 1_000,
            }],
        }
    }

    /// `--only` goes through the same `slug`, and a `None` is refused with a message rather
    /// than resolved. This is the one command that deletes, and it is where an unslugged `..`
    /// would have deleted the parent directory.
    #[test]
    fn clear_refuses_the_three_names_that_are_not_directories() {
        for name in ["", ".", "..", "   "] {
            let e = target_dir(Path::new("/state/nutsh/cache"), Some(name)).unwrap_err();
            assert!(e.to_string().contains("not a usable context name"), "{e}");
        }
        let one = target_dir(Path::new("/state/nutsh/cache"), Some("lab")).unwrap();
        assert_eq!(one, Some(PathBuf::from("/state/nutsh/cache/lab")));
        let all = target_dir(Path::new("/state/nutsh/cache"), None).unwrap();
        assert_eq!(all, None, "no --only clears the whole tree");
    }

    /// The flag on `cache clear` is `--only`, with a clap id of its own. `-c` / `--context` /
    /// `NUTSH_CONTEXT` is the global connection flag, and clap copies a global's value into
    /// every subcommand's matches - under the same id, so a shared one would have let an
    /// exported variable silently narrow the delete to one context.
    #[test]
    fn the_global_context_never_narrows_a_delete() {
        use clap::Parser as _;
        let cli = crate::Cli::try_parse_from(["nutsh", "-c", "lab", "cache", "clear"]).unwrap();
        assert_eq!(cli.conn.context.as_deref(), Some("lab"));
        assert_eq!(clear_target(cli), None, "the global is not the delete's");
        let cli = crate::Cli::try_parse_from(["nutsh", "cache", "clear", "--only", "lab"]).unwrap();
        assert_eq!(clear_target(cli).as_deref(), Some("lab"));
    }

    fn clear_target(cli: crate::Cli) -> Option<String> {
        match cli.command {
            Some(crate::Command::Cache {
                action: CacheCommand::Clear { context },
            }) => context,
            other => panic!("expected `cache clear`, got {other:?}"),
        }
    }

    /// `clear` on a real tree: one context leaves the others alone, no context takes the root,
    /// and a second run is a no-op rather than an error - the directory is already gone, which
    /// is the state the command was asked for.
    #[test]
    fn clear_removes_one_context_then_the_tree_and_says_how_many_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        for (name, text) in [("lab", "0123456789"), ("other", "abc")] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("meta.json"), text).unwrap();
        }
        assert_eq!(dir_bytes(&root), 13, "both files, walked recursively");
        assert_eq!(dir_bytes(&root.join("lab")), 10);

        assert_eq!(clear(&root, Some("lab")).unwrap(), 0);
        assert!(!root.join("lab").exists(), "the named context is gone");
        assert!(root.join("other").exists(), "and only that one");
        // Already gone: the message differs, the exit code does not.
        assert_eq!(clear(&root, Some("lab")).unwrap(), 0);

        assert_eq!(clear(&root, None).unwrap(), 0);
        assert!(!root.exists(), "no context takes the whole tree");
        assert_eq!(clear(&root, None).unwrap(), 0, "and again is still fine");
    }

    /// `info` prints per context: the identity, the age in human units, the pin count, and one
    /// line per table. It connects to nothing, so it is a pure function over what it read.
    #[test]
    fn info_reports_the_identity_the_age_the_pins_and_the_tables() {
        let text = report(
            "lab",
            &peeked(FORMAT, env!("CARGO_PKG_VERSION"), 2),
            1_000 + 90 * 60,
        );
        assert!(text.contains("lab"), "{text}");
        assert!(text.contains("admin@pc.lab.example:9440"), "{text}");
        assert!(text.contains("pc.7.6"), "{text}");
        assert!(text.contains("1h30m ago"), "{text}");
        assert!(text.contains("2 pins"), "{text}");
        assert!(text.contains("vmm.ahv.config.Vm"), "{text}");
        assert!(text.contains("100 rows"), "{text}");
        assert!(
            !text.contains("will not use it"),
            "a directory this build reads is not flagged: {text}"
        );
        // One pin is one pin.
        let one = report("lab", &peeked(FORMAT, env!("CARGO_PKG_VERSION"), 1), 1_000);
        assert!(one.contains("1 pin\n"), "{one}");
    }

    /// A directory `cache::read` would discard or step over reads exactly like a healthy one
    /// unless the line says otherwise - and then the empty first frame has no explanation.
    #[test]
    fn info_flags_a_directory_this_build_will_not_use() {
        let text = report("old", &peeked(FORMAT + 1, "9.9.9", 0), 1_000);
        assert!(
            text.contains(&format!("(format {}, nutsh 9.9.9 -", FORMAT + 1)),
            "{text}"
        );
        assert!(text.contains("this build will not use it"), "{text}");
        // Either reason alone is enough, and each names only itself.
        let text = report("old", &peeked(FORMAT, "9.9.9", 0), 1_000);
        assert!(text.contains("(nutsh 9.9.9 -"), "{text}");
        let text = report(
            "old",
            &peeked(FORMAT + 1, env!("CARGO_PKG_VERSION"), 0),
            1_000,
        );
        assert!(
            text.contains(&format!("(format {} -", FORMAT + 1)),
            "{text}"
        );
    }

    #[test]
    fn an_age_reads_in_human_units() {
        assert_eq!(age(0), "just now");
        assert_eq!(age(45), "45s ago");
        assert_eq!(age(90), "1m30s ago");
        assert_eq!(age(5_400), "1h30m ago");
        assert_eq!(age(90_000), "1d1h ago");
    }

    /// `info` grows one line per context that has a history, and says nothing for one that does
    /// not - including a file this build must not read.
    #[test]
    fn info_counts_the_history_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lab");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(history_line(&dir), None, "no file, no line");
        let write = |text: &str| std::fs::write(dir.join("history.json"), text).unwrap();
        write(r#"{"version":1,"entries":["vm","ctx lab"]}"#);
        assert_eq!(
            history_line(&dir).as_deref(),
            Some("    history: 2 lines\n")
        );
        write(r#"{"version":1,"entries":["vm"]}"#);
        assert_eq!(history_line(&dir).as_deref(), Some("    history: 1 line\n"));
        write(r#"{"version":99,"entries":["vm"]}"#);
        assert_eq!(
            history_line(&dir),
            None,
            "a format this build does not know"
        );
        assert!(dir.join("history.json").exists(), "and it is not deleted");
    }
}
