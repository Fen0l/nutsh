//! `nutsh [KIND]`: the TUI, and `--snapshot` for one headless frame.

use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use nutsh_config::{ConfigError, Secrets};
use nutsh_core::contexts::{no_stored_password, no_stored_password_cli};
use nutsh_tui::App;
use nutsh_tui::app::{Config, Mode};

use crate::ConnArgs;
use crate::contexts::{CliContexts, Overrides};
use crate::session::{self, Target};

pub(crate) struct TuiArgs {
    /// What the run opens on: a page id or a kind id, resolved by `resolve_home` - the one
    /// ranking the palette already uses for both, so `[KIND]` names a page as readily as a
    /// kind and nothing here has to know which of the two it was handed.
    pub(crate) start: String,
    pub(crate) snapshot: bool,
    pub(crate) size: (u16, u16),
    pub(crate) readonly: bool,
    pub(crate) no_cache: bool,
}

/// What startup decided: a connected app on a table, or the Contexts screen.
enum Start {
    App(App),
    /// The Contexts screen. `Some(reason)` is a run that failed - the command line asked for
    /// something and did not get it - which `--snapshot` reports on stderr and exits 3 for.
    /// `None` is the ordinary "there is nothing to connect to yet": the screen is the answer,
    /// and under `--snapshot` the frame is.
    Failed(App, Option<String>),
}

pub(crate) fn run(args: TuiArgs, conn: &ConnArgs, from_env: Option<String>) -> anyhow::Result<i32> {
    // Before anything connects: an unknown kind is a mistake in the command line, and saying
    // so costs nothing, while a round trip to Prism Central to reach the same conclusion does.
    nutsh_tui::app::resolve_home(&args.start)?;
    let rt = crate::runtime()?;
    rt.block_on(async {
        let config_path = nutsh_config::config_path();
        // Resolved once. It is both what this session connects with and, when `--host` named
        // it, the `(env)` row the Contexts screen offers; and when it cannot be resolved at
        // all, that failure is the Contexts screen's reason rather than a second lookup.
        let target = session::target(conn, &config_path);
        // Before the alternate screen and before any frame: the auto-detected skin queries the
        // terminal's background colour, and a warning printed later would land inside a frame.
        // The context is the resolved one, so a skin on `current_context` counts as much as
        // one on `--context`; the warning names the key the name came from, so a context's
        // skin is looked for where it is.
        let file = nutsh_config::load(&config_path).unwrap_or_default();
        // `--check` never gets here, and it never caches: its job is to probe every namespace
        // and report, and a cache would make it report the past.
        let caching = file.cache && !args.no_cache;
        let cache_max_age = file
            .cache_max_age
            .map_or(nutsh_core::cache::MAX_AGE, u64::from);
        if caching {
            // Before the directory this run wants is read: keep the tree from growing a
            // directory per Prism Central ever visited.
            let root = nutsh_config::cache_dir();
            // Printed, not logged. There is a subscriber now, but a TUI run writes to a file
            // nobody has been told to look in, and a sweep that failed is something this run
            // has to say out loud. The skin warnings below print at the same point for the
            // same reason - before the alternate screen, where a line on stderr is still a
            // line and not a hole in a frame.
            if let Err(e) = nutsh_core::cache::sweep(&root) {
                eprintln!("warning: cache sweep failed: {e:#}");
            }
        }
        // Before the network: the first frame must paint before anything answers. This is the
        // one caller that can still print - it runs before the alternate screen - so a name
        // that cannot be a directory is said out loud here rather than swallowed.
        let (cache, restored) = match caching.then(|| target.as_ref().ok()).flatten() {
            Some(t) => match crate::cache::open_for(t.context.as_deref(), &t.profile, cache_max_age) {
                Ok((dir, restored)) => (Some(dir), restored),
                Err(name) => {
                    eprintln!(
                        "warning: {name} is not a usable cache directory name; this session is cacheless"
                    );
                    (None, None)
                }
            },
            None => (None, None),
        };
        let context_skin = target
            .as_ref()
            .ok()
            .and_then(|t| t.context.as_deref())
            .and_then(|name| {
                let skin = file.contexts.get(name)?.skin.as_deref()?;
                Some((skin, format!("contexts.{name}.skin")))
            });
        let (skin_name, source) = match context_skin {
            Some((skin, source)) => (Some(skin), source),
            None => (file.skin.name.as_deref(), "skin.name".to_string()),
        };
        for warning in nutsh_tui::theme::validate_skin(skin_name, &file.skin.colors, &source) {
            eprintln!("warning: {warning}");
        }
        nutsh_tui::theme::install(skin_name, &file.skin.colors, file.skin.background);
        let env_target = match (&conn.host, &target) {
            (Some(_), Ok(t)) => Some((t.profile.clone(), from_env.clone())),
            _ => None,
        };
        let contexts = CliContexts::new(
            config_path.clone(),
            Secrets::default_stores(),
            env_target,
            conn.context.clone(),
            Overrides::from_args(conn, args.readonly),
        )
        // A context switch reads its own directory before connecting, on the same terms as
        // this run's: `cache = false` and `--no-cache` are properties of the run, not of the
        // context it started on.
        .caching(caching, cache_max_age);
        let config = Config {
            now: SystemTime::now(),
            config_path: Some(config_path.display().to_string()),
            // `--readonly`, the context's, and the file's; any one is enough. The context's
            // reaches the session instead, which `core::actions::reason` reads beside these.
            // An invalid rule stopped startup in `main`, so this cannot fail here.
            guardrails: nutsh_core::guardrails::Guardrails::from_config(
                file.readonly || args.readonly,
                &file.guardrails,
            )
            .unwrap_or_default(),
            snapshot: args.snapshot,
            // The file's alone: there is no flag for it. `ctrl-o` turns it off for a session
            // and `:mouse` writes the answer back here.
            mouse: file.mouse,
            // Likewise the file's, and `auto` unless it says otherwise: the terminal's height
            // decides, and `:header` writes an override back here.
            header: file.header,
        };
        let start = start(&args, target, from_env, contexts, config, cache, restored).await?;
        match (start, args.snapshot) {
            (Start::App(mut app), true) => {
                // A page waits for every pane; a table for its one subscription.
                let settled = if app.page().is_some() {
                    tokio::time::timeout(Duration::from_secs(10), app.settle_page_once()).await
                } else {
                    tokio::time::timeout(Duration::from_secs(10), app.settle_once()).await
                };
                settled.context("the first poll did not complete within 10 s")?;
                // A *second*, separate, non-fatal wait. The first one turns its timeout into an
                // error, and a wait for the warm-up built the same way would turn a slow Prism
                // Central into a failed `--snapshot` - trading one kind of non-determinism for a
                // worse one. So this one is awaited after the poll timeout has already
                // succeeded, its `Err` is discarded, and the frame is printed either way. What
                // it buys is "a frame with the names and the numbers, whenever the Prism
                // Central can answer in time"; the guarantee is bought against `mockpc`, where
                // both always arrive, by `tests/snapshot.rs`.
                let _ = tokio::time::timeout(Duration::from_secs(10), async {
                    app.settle_names_once().await;
                    app.settle_stats_once().await;
                })
                .await;
                print!("{}", app.snapshot(args.size.0, args.size.1)?);
                Ok(0)
            }
            (Start::Failed(app, reason), true) => match reason {
                Some(reason) => Err(anyhow::anyhow!("{reason}")),
                None => {
                    print!("{}", app.snapshot(args.size.0, args.size.1)?);
                    Ok(0)
                }
            },
            (Start::App(app) | Start::Failed(app, _), false) => {
                nutsh_tui::run(app).await?;
                Ok(0)
            }
        }
    })
}

/// The startup rules of the design: connect when a target and a password exist; otherwise, or
/// on failure, the Contexts screen with the reason.
async fn start(
    args: &TuiArgs,
    target: anyhow::Result<Target>,
    from_env: Option<String>,
    contexts: CliContexts,
    config: Config,
    cache: Option<std::path::PathBuf>,
    restored: Option<nutsh_core::cache::Restored>,
) -> anyhow::Result<Start> {
    let start = &args.start;
    let target = match target {
        Ok(t) => t,
        Err(e) => {
            let (message, reason) = refusal(&e);
            let app = App::disconnected(Box::new(contexts), start, message, None, config);
            return Ok(Start::Failed(app, reason));
        }
    };
    let stored = session::stored_password(&target, from_env, &contexts.secrets);
    let password = match stored.password {
        Some(p) => p,
        None => {
            // A prompt is only an answer when somebody is there to answer it and the run is
            // not one frame long: `--snapshot` on a terminal must never sit waiting for a
            // password nobody is watching for.
            let can_prompt = !args.snapshot
                && target.context.is_none()
                && std::io::IsTerminal::is_terminal(&std::io::stdin());
            if can_prompt {
                session::prompt_password(&target.profile.username, &target.profile.host)?
            } else {
                return Ok(no_password(target, stored.error, contexts, args, config));
            }
        }
    };
    let mut target = target;
    target.readonly |= args.readonly;
    let context = target.context.clone();
    match session::connect(target, &password, restored.as_ref()).await {
        Ok(session) => {
            let mut app = App::open(session, Box::new(contexts), start, config)?;
            // After `open`, before the first frame: the rows, the names, the counters and the
            // permission set are on screen before anything has answered.
            if let Some(dir) = cache {
                app.enable_cache(dir, restored);
            }
            Ok(Start::App(app))
        }
        Err(e) => {
            let text = format!("{e:#}");
            let app = App::disconnected(
                Box::new(contexts),
                start,
                Some(text.clone()),
                context.as_deref(),
                config,
            );
            Ok(Start::Failed(app, Some(text)))
        }
    }
}

/// There is a target but no password for it.
///
/// Interactively a named context is not a failure - the screen opens its login box and the
/// user types one, which is the whole point of having the screen - while a `--host` target has
/// no row to log in from, so there is nowhere for the screen to take a password. Under
/// `--snapshot` neither is: nobody is going to type into a frame, so both are a run that asked
/// for a session and did not get one, and both say so on stderr and exit 3.
fn no_password(
    target: Target,
    store_error: Option<String>,
    contexts: CliContexts,
    args: &TuiArgs,
    config: Config,
) -> Start {
    let start = &args.start;
    let Some(name) = target.context.clone() else {
        let text = join(
            store_error,
            format!(
                "no password for {}@{}: set NUTSH_PASSWORD, or add a context and run `nutsh ctx login`",
                target.profile.username, target.profile.host
            ),
        );
        let app = App::disconnected(Box::new(contexts), start, Some(text.clone()), None, config);
        return Start::Failed(app, Some(text));
    };
    // Two audiences, one fact: the box says "or type it here" because the box is right there
    // to type into, and stderr - where nobody is going to type anything - says how to give
    // the password to the next run instead.
    let why = join(store_error.clone(), no_stored_password(&name));
    let reason = args
        .snapshot
        .then(|| join(store_error, no_stored_password_cli(&name)));
    let mut app = App::disconnected(Box::new(contexts), start, None, Some(&name), config);
    // The same box `:ctx NAME` opens, for the same reason: the screen asks for the password
    // instead of refusing, and says why inside the box, which covers the rows the reason
    // would otherwise sit under.
    app.screen.open_login(name, Some(why));
    app.mode = Mode::Form;
    Start::Failed(app, reason)
}

/// A secret store that could not be read, above the advice for the password it therefore does
/// not have: two different problems, and the first is why the second happened.
fn join(store_error: Option<String>, advice: String) -> String {
    match store_error {
        Some(e) => format!("{e}\n{advice}"),
        None => advice,
    }
}

/// What startup should say about a target it could not resolve, as `(screen message, failure
/// reason)`.
///
/// The Contexts screen explains two of these for itself, and saying them again above the rows
/// would only say them twice: an empty screen is what "no context selected" looks like, and a
/// config file that will not load becomes the screen's own `list_error`. "Unknown context" is
/// the one it cannot explain - the file loads, the rows are there, and none of them is the one
/// that was asked for - so that reason is passed in.
fn refusal(e: &anyhow::Error) -> (Option<String>, Option<String>) {
    let text = format!("{e:#}");
    match e.downcast_ref::<ConfigError>() {
        // Nothing was asked for and nothing was found: not a failure, just an empty screen.
        Some(ConfigError::NoContext(_)) => (None, None),
        Some(c) if unreadable(c) => (None, Some(text)),
        _ => (Some(text.clone()), Some(text)),
    }
}

/// Errors that stop the config file loading at all, which is what the screen's `list_error`
/// reports on its own.
fn unreadable(e: &ConfigError) -> bool {
    matches!(
        e,
        ConfigError::Read { .. }
            | ConfigError::Parse { .. }
            | ConfigError::PasswordInFile { .. }
            | ConfigError::PasswordKey { .. }
            | ConfigError::InvalidName(_)
    )
}
