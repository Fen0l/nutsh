//! Tracing, switched on or off for the whole program.
//!
//! Seventeen `tracing` events were already written across this workspace before anything
//! subscribed to them, so all of this is a switch rather than an instrumentation pass: the
//! pacing waits, the valve latching, the cache decisions and the per-request event in
//! `nutsh_prism::client` are what a level turns on.
//!
//! **What may be written down is the constraint here, not what is.** A password never reaches
//! argv, a child's environment, the config file, the journal, a cache file or a fixture, and a
//! log is a new way out for all three of the credentials this program handles: the
//! `Authorization` header, the session cookies Prism Central issues, and a request body. Two
//! rules keep it shut. Nothing in this workspace logs a header, a cookie or a body - see
//! `Client::trace` for the one event that comes close and what it leaves out - and, whatever
//! `NUTSH_LOG` says, only this program's own crates can reach the writer: `hyper` and `h2` log
//! frames, and an HPACK frame is a header dump with the credential still in it. `tests/log.rs`
//! is the proof of both, and it is the deliverable more than the feature is.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use nutsh_config::LogLevel;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _, filter, fmt, reload};

use crate::contexts::Source;

/// The crates whose events may be written. Anything else is dropped at the writer whatever the
/// filter says, which is what makes `NUTSH_LOG=trace` safe to type.
const TARGETS: [&str; 6] = [
    "nutsh",
    "nutsh_catalog",
    "nutsh_config",
    "nutsh_core",
    "nutsh_prism",
    "nutsh_tui",
];

/// What one log file may grow to before it is rolled over the one kept beside it. Two files, so
/// an overnight session costs at most twice this and the recent lines - the ones somebody
/// turned logging on to read - are the ones that survive.
const CAP: u64 = 4 * 1024 * 1024;

/// The file name under [`nutsh_config::logs_dir`]; the rolled one takes a `.1`.
const FILE: &str = "nutsh.log";

/// Where the lines go, which is a property of the run and not of the user's wishes.
///
/// The TUI owns the alternate screen: a line on stderr there is a hole in the frame, not a
/// message, so it writes to a file instead. `--snapshot` renders a frame to stdout and is the
/// same case. Everything else - `--check`, `--info`, `ctx`, `cache` - has a terminal it is
/// already writing to and uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    Stderr,
    File,
}

/// What a run decided about logging, and what the settings screen draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logging {
    /// One of the six words, or `None` when `NUTSH_LOG` named a `tracing_subscriber` filter
    /// string instead. `:log` always sets a word, so `None` only ever describes a startup.
    pub level: Option<LogLevel>,
    /// What was actually installed, which is the level word's directives or the filter string
    /// as typed. Shown by the settings screen when there is no word to show.
    pub spec: String,
    pub source: Source,
    /// The file being written to, or `None` for a run whose lines go to stderr.
    pub path: Option<PathBuf>,
    /// A `NUTSH_LOG` that could not be read, for the caller to print. The run carries on at the
    /// level the file or the default asked for: a typo in an environment variable is not a
    /// reason to refuse to start.
    pub warning: Option<String>,
}

impl Logging {
    /// What the settings screen puts in the value column: the word, or the filter string when
    /// somebody asked for one by hand.
    pub fn value(&self) -> String {
        self.level
            .map_or_else(|| self.spec.clone(), |l| l.to_string())
    }

    /// Whether anything is being written at all.
    pub fn on(&self) -> bool {
        self.level != Some(LogLevel::Off)
    }

    fn off() -> Logging {
        Logging {
            level: Some(LogLevel::Off),
            spec: LogLevel::Off.word().to_string(),
            source: Source::Default,
            path: None,
            warning: None,
        }
    }
}

type Handle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;

/// The one way the level moves after startup. `None` until [`install`] runs, which is what
/// makes `:log` in a process that never installed a subscriber say so rather than pretend.
static RELOAD: OnceLock<Handle> = OnceLock::new();

static STATE: RwLock<Option<Logging>> = RwLock::new(None);

/// What this run is doing, or `None` in a process that never installed a subscriber - the unit
/// tests, and anything else that links this crate without being the program.
pub fn state() -> Option<Logging> {
    STATE.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The file a run with no terminal of its own writes to. Stated even when nothing is being
/// written, because `nutsh --info` has to be able to say where the log *would* go: a user who
/// is about to turn logging on wants the path before there is a file at it.
pub fn path() -> PathBuf {
    nutsh_config::logs_dir().join(FILE)
}

/// Install the subscriber for this process, once, and answer what it decided.
///
/// Precedence is the usual one and is settled here rather than by any caller: `NUTSH_LOG` beats
/// the config file's `log`, which beats off. The subscriber is installed whatever the answer,
/// with the filter set to `off` when nobody asked for anything - that costs nothing, writes
/// nothing and creates no file, and it is what lets `:log` raise the level mid-session without
/// a second way of starting one.
pub fn install(sink: Sink) -> Logging {
    let from_file = nutsh_config::load(&nutsh_config::config_path())
        .map(|cfg| cfg.log)
        .unwrap_or_default();
    let (mut decided, filter) = decide(std::env::var("NUTSH_LOG").ok().as_deref(), from_file);
    decided.path = (sink == Sink::File).then(path);
    let writer = decided.path.clone().map(Rolling::shared);
    // A level somebody asked for means a directory they expect to find the file in, so it is
    // made now rather than at the first line: an event that cannot be written has nowhere to
    // report that it could not be.
    if let (Some(dir), true) = (decided.path.as_ref().and_then(|p| p.parent()), decided.on())
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        decided.warning = Some(format!("{}: {e}", dir.display()));
    }

    let (reloadable, handle) = reload::Layer::new(filter);
    // The second of the two rules, and the one that does not depend on anybody's taste in
    // filter strings: an event from a crate that is not ours never reaches a writer.
    let ours = filter::filter_fn(|meta| is_ours(meta.target()));
    let registry = tracing_subscriber::registry().with(reloadable);
    let installed = match writer.clone() {
        Some(file) => registry
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .with_writer(file)
                    .with_filter(ours),
            )
            .try_init(),
        None => registry
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::io::stderr)
                    .with_filter(ours),
            )
            .try_init(),
    };
    if let Err(e) = installed {
        // Something else in this process got there first. Say so once and go quiet: two
        // subscribers is not a thing that can be half-fixed here.
        return Logging {
            warning: Some(format!("logging is already installed: {e}")),
            ..Logging::off()
        };
    }
    let _ = RELOAD.set(handle);
    remember(decided.clone());
    if decided.on() {
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            level = %decided.value(),
            "nutsh starting"
        );
    }
    decided
}

/// Move the level for the rest of this session, and answer what is now in force. The source
/// becomes `this session`: a settings row that said `config file` over a level the user changed
/// ten seconds ago is exactly the lie that screen exists to stop.
///
/// A process with no subscriber installed is an error rather than a silent success. The only
/// ones are the unit tests, and a status line saying the level changed when nothing was
/// listening would be worse than one saying why it did not.
pub fn set(level: LogLevel) -> anyhow::Result<Logging> {
    let handle = RELOAD
        .get()
        .ok_or_else(|| anyhow::anyhow!("this run installed no logging"))?;
    let mut now = state().unwrap_or_else(Logging::off);
    if let (Some(dir), true) = (
        now.path.as_ref().and_then(|p| p.parent()),
        level != LogLevel::Off,
    ) {
        std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    }
    now.spec = directives(level);
    handle.reload(EnvFilter::new(&now.spec))?;
    now.level = Some(level);
    now.source = Source::Session;
    now.warning = None;
    remember(now.clone());
    Ok(now)
}

fn remember(state: Logging) {
    *STATE.write().unwrap_or_else(PoisonError::into_inner) = Some(state);
}

/// The precedence, with no globals in it so a test can read it.
fn decide(env: Option<&str>, from_file: LogLevel) -> (Logging, EnvFilter) {
    let file = |spec: String, level: LogLevel| Logging {
        level: Some(level),
        spec,
        source: if level == from_file && level != LogLevel::Off {
            Source::File
        } else {
            Source::Default
        },
        path: None,
        warning: None,
    };
    let Some(asked) = env.map(str::trim).filter(|s| !s.is_empty()) else {
        let level = from_file;
        return (
            file(directives(level), level),
            EnvFilter::new(directives(level)),
        );
    };
    if let Some(level) = LogLevel::parse(asked) {
        let spec = directives(level);
        let filter = EnvFilter::new(&spec);
        return (
            Logging {
                level: Some(level),
                spec,
                source: Source::Flag("NUTSH_LOG"),
                path: None,
                warning: None,
            },
            filter,
        );
    }
    // Not one of the six words, so `EnvFilter` is asked to read it as what it is: a filter
    // string, which is how `NUTSH_LOG=nutsh_prism=debug` asks for the requests and nothing
    // else. It can still only reach this program's own crates.
    match EnvFilter::try_new(asked) {
        Ok(filter) => (
            Logging {
                level: None,
                spec: asked.to_string(),
                source: Source::Flag("NUTSH_LOG"),
                path: None,
                // A bare word `EnvFilter` does not know is a *target* name, not a refusal, so
                // `NUTSH_LOG=debg` installs cleanly and writes nothing for the rest of the
                // session. Somebody who turned logging on to read it has to be told that.
                warning: (!reaches_us(asked)).then(|| {
                    format!(
                        "NUTSH_LOG={asked} names no nutsh crate, so nothing will be written; \
                         the levels are off, error, warn, info, debug and trace"
                    )
                }),
            },
            filter,
        ),
        Err(e) => {
            let level = from_file;
            let mut fallback = file(directives(level), level);
            fallback.warning = Some(format!(
                "NUTSH_LOG={asked} is neither a level (off, error, warn, info, debug, trace) nor a filter: {e}"
            ));
            (fallback, EnvFilter::new(directives(level)))
        }
    }
}

/// One level word, as the directives that give it to this program's crates and to nothing else.
///
/// Deliberately not the bare word. `EnvFilter::new("debug")` is a global default that turns on
/// every crate in the tree, `hyper` and `h2` among them, and those two log the frames a request
/// is made of - which is to say its headers, which is to say the credential.
fn directives(level: LogLevel) -> String {
    if level == LogLevel::Off {
        return LogLevel::Off.word().to_string();
    }
    TARGETS
        .iter()
        .map(|t| format!("{t}={level}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether an event's target is one of this program's crates. By path segment, not by prefix:
/// `nutsh_prism::client` is ours and a crate called `nutshell` would not be.
fn is_ours(target: &str) -> bool {
    let root = target.split("::").next().unwrap_or(target);
    TARGETS.contains(&root)
}

/// Whether a hand-written filter string could turn anything on at all: at least one directive
/// naming a crate of ours, or a bare level, which is a default for every target there is.
fn reaches_us(spec: &str) -> bool {
    spec.split(',').any(|directive| {
        let target = directive
            .split('=')
            .next()
            .unwrap_or(directive)
            .split('[')
            .next()
            .unwrap_or(directive)
            .trim();
        target.is_empty() || LogLevel::parse(target).is_some() || is_ours(target)
    })
}

/// The log file, and the cap that keeps a session nobody is watching from filling a disk.
///
/// Opened at the first line rather than at startup, which is what lets a run at `off` - every
/// run nobody asked anything of - leave no file behind at all.
struct Rolling {
    path: PathBuf,
    file: Option<std::fs::File>,
    written: u64,
}

impl Rolling {
    fn shared(path: PathBuf) -> LogFile {
        LogFile(Arc::new(Mutex::new(Rolling {
            path,
            file: None,
            written: 0,
        })))
    }

    /// Appends, so a log spans the runs somebody is comparing. The cap is what keeps that safe.
    fn open(&mut self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        // A log carries hostnames, usernames and entity ids: the same 0600 the config file and
        // the secret store are written with.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(&self.path)?;
        self.written = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    /// The file is full: it becomes the one kept beside it and a fresh one starts. Whatever was
    /// in `.1` goes, which is the bound.
    fn roll(&mut self) -> std::io::Result<()> {
        self.file = None;
        let name = self.path.file_name().unwrap_or_default().to_os_string();
        let mut rolled = name.clone();
        rolled.push(".1");
        std::fs::rename(&self.path, self.path.with_file_name(rolled))?;
        self.open()
    }
}

impl Write for Rolling {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.file.is_none() {
            self.open()?;
        }
        if self.written + buf.len() as u64 > CAP {
            self.roll()?;
        }
        let file = self.file.as_mut().expect("opened above");
        let n = file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

/// The handle the `fmt` layer writes through. By hand rather than leaning on the blanket
/// `MakeWriter` impls, because the roll has to happen under the same lock as the write.
#[derive(Clone)]
struct LogFile(Arc<Mutex<Rolling>>);

impl<'a> fmt::MakeWriter<'a> for LogFile {
    type Writer = Locked<'a>;

    fn make_writer(&'a self) -> Locked<'a> {
        Locked(self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

struct Locked<'a>(MutexGuard<'a, Rolling>);

impl Write for Locked<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment beats the file, the file beats the default, and the default is off.
    #[test]
    fn the_environment_beats_the_file_and_the_file_beats_off() {
        let (off, _) = decide(None, LogLevel::Off);
        assert_eq!(off.level, Some(LogLevel::Off));
        assert_eq!(off.source, Source::Default);
        assert!(!off.on());

        let (from_file, _) = decide(None, LogLevel::Debug);
        assert_eq!(from_file.level, Some(LogLevel::Debug));
        assert_eq!(from_file.source, Source::File);

        let (from_env, _) = decide(Some("trace"), LogLevel::Debug);
        assert_eq!(from_env.level, Some(LogLevel::Trace));
        assert_eq!(from_env.source, Source::Flag("NUTSH_LOG"));

        // An empty variable is not an answer; a file that says `log = "info"` still is.
        let (empty, _) = decide(Some("  "), LogLevel::Info);
        assert_eq!(empty.level, Some(LogLevel::Info));
        assert_eq!(empty.source, Source::File);

        // `NUTSH_LOG=off` over a file that asked for a log is a run with no log.
        let (silenced, _) = decide(Some("off"), LogLevel::Trace);
        assert_eq!(silenced.level, Some(LogLevel::Off));
        assert!(!silenced.on());
    }

    /// A filter string is taken as one, and a typo is a warning rather than a refusal to start.
    #[test]
    fn a_filter_string_is_kept_and_a_typo_falls_back_to_the_file() {
        let (spelled, _) = decide(Some("nutsh_prism=debug"), LogLevel::Off);
        assert_eq!(spelled.level, None);
        assert_eq!(spelled.value(), "nutsh_prism=debug");
        assert_eq!(spelled.source, Source::Flag("NUTSH_LOG"));

        let (bad, _) = decide(Some("=?="), LogLevel::Warn);
        assert_eq!(bad.level, Some(LogLevel::Warn), "the file still decides");
        assert!(bad.warning.is_some(), "and the typo is reported");

        // `EnvFilter` reads a word it does not know as a *target*, so a misspelt level installs
        // cleanly and writes nothing. Silence is the one answer that must not be silent.
        let (typo, _) = decide(Some("debg"), LogLevel::Off);
        assert!(typo.warning.is_some(), "a level nobody spelled right");
        assert!(reaches_us("nutsh_prism::client=debug,nutsh_core=info"));
        assert!(
            reaches_us("trace"),
            "a bare level is a default for everything"
        );
        assert!(!reaches_us("h2=trace,hyper=trace"));
    }

    /// Every level a user can ask for names this program's crates and never a bare global
    /// default: `hyper` and `h2` log headers, and a log that carried a header would carry a
    /// credential.
    #[test]
    fn no_level_ever_turns_on_a_crate_that_is_not_ours() {
        for level in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            let spec = directives(level);
            assert!(spec.contains(&format!("nutsh_prism={level}")), "{spec}");
            for directive in spec.split(',') {
                let target = directive.split('=').next().unwrap();
                assert!(TARGETS.contains(&target), "{spec} reaches {target}");
            }
        }
        assert_eq!(directives(LogLevel::Off), "off");
    }

    /// And whatever the filter says, the writer only takes our own events. This is the rule
    /// that holds when somebody types `NUTSH_LOG=trace` as a filter string rather than a word.
    #[test]
    fn only_our_own_crates_reach_the_writer() {
        assert!(is_ours("nutsh_prism::client"));
        assert!(is_ours("nutsh_core"));
        assert!(is_ours("nutsh"));
        for theirs in [
            "h2::codec",
            "hyper",
            "rustls",
            "reqwest::async_impl",
            "nutshell",
        ] {
            assert!(!is_ours(theirs), "{theirs}");
        }
    }

    /// Two files and no more: the second time the cap is passed, the first file's lines are the
    /// ones that go.
    #[test]
    fn the_file_rolls_at_the_cap_and_keeps_one_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join(FILE);
        let mut rolling = Rolling {
            path: path.clone(),
            file: None,
            written: 0,
        };
        assert!(!path.exists(), "nothing is written until a line is");
        let line = vec![b'x'; 64 * 1024];
        for _ in 0..(CAP / line.len() as u64 * 2 + 4) {
            rolling.write_all(&line).unwrap();
        }
        rolling.flush().unwrap();
        let rolled = path.with_file_name(format!("{FILE}.1"));
        assert!(path.exists() && rolled.exists());
        let total =
            std::fs::metadata(&path).unwrap().len() + std::fs::metadata(&rolled).unwrap().len();
        assert!(total <= 2 * CAP, "{total} bytes over the {CAP} cap");
        let entries = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 2, "two files, however long the session runs");
    }
}
