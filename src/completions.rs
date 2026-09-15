//! Shell completion: the flags and subcommands from clap, the kinds from the catalog, the
//! context names from the config file. CLI only, and never near the keyring.

use clap::CommandFactory as _;
use clap_complete::CompletionCandidate;
use clap_complete::env::{CompleteEnv, Shells};

/// The completion request, if this run is one: `COMPLETE=<shell> nutsh -- …`, which the
/// registration script sends. Answers it and exits; an ordinary run comes straight back.
/// First thing in `main`, before anything writes to stdout.
pub(crate) fn maybe_complete() {
    CompleteEnv::with_factory(crate::Cli::command).complete();
}

/// `nutsh completions <shell>`: the script that wires the shell to [`maybe_complete`].
pub(crate) fn print(shell: &str) -> anyhow::Result<i32> {
    let shells = Shells::builtins();
    let Some(completer) = shells.completer(shell) else {
        let known: Vec<&str> = shells.names().collect();
        anyhow::bail!(
            "{shell} is not a shell I know; try one of {}",
            known.join(", ")
        );
    };
    // The binary as the shell should call it back: where it is now, so a build in `target/`
    // completes without being on `PATH`.
    let bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "nutsh".to_string());
    let mut out = std::io::stdout().lock();
    completer.write_registration("COMPLETE", "nutsh", "nutsh", &bin, &mut out)?;
    Ok(0)
}

/// What `[KIND]` takes: every page, every kind id, every alias, each with what it opens.
pub(crate) fn kinds() -> Vec<CompletionCandidate> {
    let mut out = Vec::new();
    for page in nutsh_catalog::PAGES {
        out.push(CompletionCandidate::new(page.id).help(Some(page.title.into())));
    }
    for kind in nutsh_catalog::KINDS {
        out.push(CompletionCandidate::new(kind.id).help(Some(kind.display.into())));
        for alias in kind.aliases {
            out.push(CompletionCandidate::new(*alias).help(Some(kind.display.into())));
        }
    }
    out
}

/// What `-c` and `cache clear --only` take: the context names in the config file. The file
/// and nothing else: a name is not a secret, and the secret store is never opened here.
pub(crate) fn contexts() -> Vec<CompletionCandidate> {
    let Ok(config) = nutsh_config::load(&nutsh_config::config_path()) else {
        return Vec::new();
    };
    config
        .contexts
        .iter()
        .map(|(name, c)| {
            CompletionCandidate::new(name.as_str())
                .help(Some(format!("{}@{}", c.username, c.host).into()))
        })
        .collect()
}
