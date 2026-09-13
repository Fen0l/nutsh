//! nutsh command-line entry point.

mod cache;
mod check;
mod contexts;
mod ctx;
mod session;
mod tui;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "nutsh",
    version,
    about = "A Nutanix TUI that tells you why it's broken"
)]
struct Cli {
    /// Kind or page to open (default: the Dashboard); any catalog id, alias, page name, or
    /// display-name prefix
    #[arg(conflicts_with_all = ["check", "info"])]
    kind: Option<String>,
    #[command(flatten)]
    conn: ConnArgs,
    /// Connect, probe every API namespace, print a status table, and exit
    #[arg(long)]
    check: bool,
    /// Print build and catalog information and exit
    #[arg(long)]
    info: bool,
    /// Render one frame to stdout and exit (waits for the first poll)
    #[arg(long, conflicts_with_all = ["check", "info"])]
    snapshot: bool,
    // No `default_value`: the default is applied where it is used, so that "the user asked
    // for a size" and "nobody asked" stay distinguishable - which is what lets `--size` be
    // refused alongside a subcommand it would mean nothing to.
    /// Frame size for --snapshot, COLSxROWS (default 120x40)
    #[arg(long, value_name = "COLSxROWS", value_parser = parse_size, conflicts_with_all = ["check", "info"])]
    size: Option<(u16, u16)>,
    // Deliberately not `global`: `ctx add --readonly` means "store this context read-only",
    // and a global flag of the same name would be that flag, silently writing the property
    // into the config file whenever a session was only meant to be read-only for one run.
    //
    // These two are the TUI-only flags that `tui_argument` does *not* refuse beside a
    // subcommand: both are accepted and ignored there, deliberately. `--readonly` has to be -
    // `readonly_before_a_subcommand_is_not_ctx_adds_readonly` pins that `nutsh --readonly ctx
    // add ...` succeeds and writes no `readonly = true` - and singling out `--no-cache` would
    // make one of a matched pair an error for no reason a user could infer. Unlike `[KIND]`,
    // `--snapshot` and `--size`, neither has a second reading: nothing is dropped by running
    // the subcommand, because neither says anything about what the subcommand does.
    /// Refuse every mutation in this session
    #[arg(long)]
    readonly: bool,
    /// Do not read or write the cache for this run
    #[arg(long)]
    no_cache: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

/// The frame `--size` may ask for. The lower bound keeps a zero-sized frame from parsing; the
/// upper one keeps a typo from asking for a buffer big enough to abort the process on
/// allocation, which is what an unbounded `u16` here would do.
const SIZE_RANGE: std::ops::RangeInclusive<u16> = 1..=1000;

/// The frame `--snapshot` renders when nobody asked for a size: a wide terminal, tall enough
/// for a screenful of rows.
const DEFAULT_SIZE: (u16, u16) = (120, 40);

/// `--size 120x40`. Both halves must be numbers in [`SIZE_RANGE`], so that a typo is refused
/// by clap with the shape and the range it wanted.
fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| "expected COLSxROWS, e.g. 120x40".to_string())?;
    Ok((half(w, "columns")?, half(h, "rows")?))
}

fn half(text: &str, what: &str) -> Result<u16, String> {
    let bad = || {
        format!(
            "{what} must be a number from {} to {}",
            SIZE_RANGE.start(),
            SIZE_RANGE.end()
        )
    };
    let n: u16 = text.parse().map_err(|_| bad())?;
    if SIZE_RANGE.contains(&n) {
        Ok(n)
    } else {
        Err(bad())
    }
}

/// How to reach a Prism Central. `--host` bypasses contexts; otherwise a context is used.
#[derive(Args, Debug, Clone)]
pub(crate) struct ConnArgs {
    /// Named context from the config file (default: NUTSH_CONTEXT, then current_context)
    #[arg(short = 'c', long, env = "NUTSH_CONTEXT", global = true)]
    pub(crate) context: Option<String>,
    /// Prism Central hostname or IP (bypasses contexts)
    #[arg(short = 'H', long, env = "NUTSH_HOST", global = true)]
    pub(crate) host: Option<String>,
    /// Prism Central port
    #[arg(short = 'P', long, env = "NUTSH_PORT", global = true)]
    pub(crate) port: Option<u16>,
    /// Username
    #[arg(short = 'u', long, env = "NUTSH_USERNAME", global = true)]
    pub(crate) username: Option<String>,
    /// Skip TLS certificate verification (the session is marked [insecure])
    #[arg(long, global = true)]
    pub(crate) insecure: bool,
    /// PEM bundle with additional trusted CA certificates
    #[arg(long, value_name = "FILE", global = true)]
    pub(crate) ca_bundle: Option<PathBuf>,
    /// Use plain HTTP (local test servers only; refused for non-loopback hosts)
    #[arg(long, hide = true, global = true)]
    pub(crate) plain_http: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage named Prism Central contexts
    Ctx {
        #[command(subcommand)]
        action: ctx::CtxCommand,
    },
    /// Inspect or remove the on-disk cache
    Cache {
        #[command(subcommand)]
        action: cache::CacheCommand,
    },
}

fn main() {
    // Taking the password out of the environment is the first thing this process does, before
    // clap and before any branch, while this is still the only thread: `remove_var` is only
    // sound single-threaded, and the secret must be gone before anything could fork and
    // inherit it. Every command path - the TUI included - therefore starts from a scrubbed
    // environment. The interactive prompt is deferred until the target is known to be usable,
    // so a bad invocation reports what is wrong instead of asking for a password.
    let from_env = match env_password() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(3);
        }
    };
    let cli = Cli::parse();
    // Before any branch can want to say something. Where the lines go is a property of the run
    // and not of what the user asked for: the TUI and `--snapshot` own the screen, so theirs go
    // to a file, and every other path has a terminal it is already writing to.
    let logging = nutsh_core::log::install(if cli.command.is_none() && !cli.check && !cli.info {
        nutsh_core::log::Sink::File
    } else {
        nutsh_core::log::Sink::Stderr
    });
    if let Some(warning) = &logging.warning {
        eprintln!("warning: {warning}");
    }
    if cli.info {
        print_info(&logging);
        return;
    }
    // clap can rule the TUI's arguments out against `--check` and `--info`, but not against a
    // subcommand: `nutsh vm ctx list` parses as both, and running `ctx list` while ignoring
    // what the user typed for the TUI is the one outcome worth refusing.
    if cli.command.is_some()
        && let Some(what) = tui_argument(&cli)
    {
        use clap::CommandFactory as _;
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                format!("the argument {what} cannot be used with a subcommand"),
            )
            .exit();
    }
    // Before anything connects: a guardrail file that does not convert is a startup error
    // naming the value, not a rule that quietly does nothing.
    if let Err(e) = validate_guardrails() {
        eprintln!("error: {e}");
        std::process::exit(3);
    }
    // One `match` rather than a chain of `if let`s: the two subcommands are arms of one
    // `Option<Command>`, and a second `if let` would be re-testing a value the first had moved.
    let result = match cli.command {
        Some(Command::Ctx { action }) => ctx::run(action, &cli.conn, from_env),
        // Neither cache subcommand connects to anything, so neither takes a password.
        Some(Command::Cache { action }) => cache::run(action),
        None if cli.check => check::run(&cli.conn, from_env),
        None => tui::run(
            tui::TuiArgs {
                // The Dashboard, not the VM table: a bare run opens the overview - what
                // Prism Central itself opens on - and a kind is what a user asks for by
                // name. `resolve_home` takes a page id and a kind id in one ranking, so
                // the default is a page without `[KIND]` having to be able to say so.
                // The Dashboard cannot be hidden (`sidebar::protected`), so this cannot
                // land on a view the menu has dropped.
                start: cli.kind.unwrap_or_else(|| "dashboard".into()),
                snapshot: cli.snapshot,
                size: cli.size.unwrap_or(DEFAULT_SIZE),
                readonly: cli.readonly,
                no_cache: cli.no_cache,
            },
            &cli.conn,
            from_env,
        ),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(3);
        }
    }
}

/// The first argument on this command line that only the TUI could act on, named as clap
/// would name it in a usage error. `--readonly` and `--no-cache` are TUI-only too and are
/// deliberately not here: see the comment above their declarations.
fn tui_argument(cli: &Cli) -> Option<String> {
    if let Some(kind) = &cli.kind {
        return Some(format!("'[KIND]' ({kind})"));
    }
    if cli.snapshot {
        return Some("'--snapshot'".to_string());
    }
    cli.size.map(|_| "'--size <COLSxROWS>'".to_string())
}

fn print_info(logging: &nutsh_core::log::Logging) {
    println!("nutsh {}", env!("CARGO_PKG_VERSION"));
    println!(
        "target: {}-{}",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    let actions: usize = nutsh_catalog::KINDS.iter().map(|k| k.actions.len()).sum();
    println!(
        "catalog: {} namespaces, {} kinds, {} actions",
        nutsh_catalog::NAMESPACES.len(),
        nutsh_catalog::KINDS.len(),
        actions
    );
    let (mut direct, mut from_parent, mut greyed) = (0, 0, 0);
    for k in nutsh_catalog::KINDS {
        match nutsh_catalog::reach(k) {
            nutsh_catalog::Reach::Direct => direct += 1,
            nutsh_catalog::Reach::FromParent(_) => from_parent += 1,
            nutsh_catalog::Reach::NeedsParameter => greyed += 1,
        }
    }
    println!("reach: {direct} direct, {from_parent} from a parent, {greyed} need a parameter");
    let path = nutsh_config::config_path();
    println!("config: {}", path.display());
    // The path even when nothing is being written: somebody about to turn logging on wants to
    // know where to look before there is a file to look at.
    println!(
        "log: {} ({}), {}",
        logging.value(),
        logging.source.label(),
        nutsh_core::log::path().display()
    );
    // The file `config:` just named: one that does not parse is the fact to print, not a
    // default to read the skin from.
    match nutsh_config::load(&path) {
        Ok(file) => {
            println!(
                "skin: {} ({:?} depth)",
                file.skin.name.as_deref().unwrap_or("auto"),
                nutsh_tui::theme::detect_depth()
            );
            for warning in nutsh_tui::theme::validate_skin(
                file.skin.name.as_deref(),
                &file.skin.colors,
                "skin.name",
            ) {
                println!("warning: {warning}");
            }
        }
        Err(e) => println!("warning: config not read: {e:#}"),
    }
}

/// Read the config file's `[[guardrails]]` and convert them, for the error only. Every load
/// error is fatal here - a parse error, an unknown key, a password key, an invalid name - not
/// only a rule that does not convert: the TUI reads the file again when it builds its session
/// and falls back to defaults if it cannot, so it cannot tell a broken file from an empty one,
/// and a broken safety file must have stopped the process before then. This runs before every
/// command path, so `nutsh ctx list` and `--check` fail on a broken file too. It also makes the
/// Contexts screen's own report of an unreadable file (`tui::refusal`'s `unreadable` arm)
/// unreachable from this binary; the TUI task retires it.
fn validate_guardrails() -> anyhow::Result<()> {
    let path = nutsh_config::config_path();
    let cfg = nutsh_config::load(&path)?;
    nutsh_core::guardrails::Guardrails::from_config(cfg.readonly, &cfg.guardrails)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(())
}

/// Takes `NUTSH_PASSWORD` out of this process's environment, so that child processes do not
/// inherit it, and returns it. `None` means the caller should look elsewhere; a value that is
/// not UTF-8 is an error rather than a silent fall-through.
///
/// Must be called from the top of `main`, before the runtime or any other thread exists.
fn env_password() -> anyhow::Result<Option<String>> {
    let Some(raw) = std::env::var_os("NUTSH_PASSWORD") else {
        return Ok(None);
    };
    // SAFETY: called from `main` before the tokio runtime or any other thread is
    // created, so no other thread can be reading the environment concurrently.
    unsafe { std::env::remove_var("NUTSH_PASSWORD") };
    raw.into_string()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("NUTSH_PASSWORD is not valid UTF-8"))
}

pub(crate) fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Runtime::new()?)
}
