//! XDG-style locations. `NUTSH_CONFIG` overrides the config file; `XDG_CONFIG_HOME` and
//! `XDG_STATE_HOME` override the base directories; otherwise `~/.config` and `~/.local/state`.

use std::path::PathBuf;

pub fn config_path() -> PathBuf {
    config_path_from(
        std::env::var_os("NUTSH_CONFIG").map(PathBuf::from),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        &home(),
    )
}

pub fn secrets_dir() -> PathBuf {
    secrets_dir_from(xdg_state(), &home())
}

pub fn cache_dir() -> PathBuf {
    cache_dir_from(xdg_state(), &home())
}

/// Where the log goes when the run cannot use stderr. Beside the cache rather than inside it:
/// `nutsh cache clear` removes a tree it owns, and a log of what went wrong is the one thing
/// that must survive the clearing somebody does while trying to fix it.
pub fn logs_dir() -> PathBuf {
    logs_dir_from(xdg_state(), &home())
}

fn xdg_state() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME").map(PathBuf::from)
}

fn home() -> String {
    std::env::var_os("HOME")
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string())
}

pub(crate) fn config_path_from(
    explicit: Option<PathBuf>,
    xdg_config: Option<PathBuf>,
    home: &str,
) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    let base = xdg_config.unwrap_or_else(|| PathBuf::from(home).join(".config"));
    base.join("nutsh").join("config.toml")
}

/// `$XDG_STATE_HOME/nutsh`, or `~/.local/state/nutsh`. Stated once, so the XDG fallback cannot
/// be changed for the secrets and forgotten for the cache.
fn state_dir(xdg_state: Option<PathBuf>, home: &str) -> PathBuf {
    xdg_state
        .unwrap_or_else(|| PathBuf::from(home).join(".local").join("state"))
        .join("nutsh")
}

pub(crate) fn secrets_dir_from(xdg_state: Option<PathBuf>, home: &str) -> PathBuf {
    state_dir(xdg_state, home).join("secrets")
}

pub(crate) fn cache_dir_from(xdg_state: Option<PathBuf>, home: &str) -> PathBuf {
    state_dir(xdg_state, home).join("cache")
}

pub(crate) fn logs_dir_from(xdg_state: Option<PathBuf>, home: &str) -> PathBuf {
    state_dir(xdg_state, home).join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_defaults_apply() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(
            config_path_from(None, p("/x"), "/home/u"),
            PathBuf::from("/x/nutsh/config.toml")
        );
        assert_eq!(
            config_path_from(None, None, "/home/u"),
            PathBuf::from("/home/u/.config/nutsh/config.toml")
        );
        assert_eq!(
            config_path_from(p("/cfg.toml"), p("/x"), "/home/u"),
            PathBuf::from("/cfg.toml")
        );
        assert_eq!(
            secrets_dir_from(p("/st"), "/home/u"),
            PathBuf::from("/st/nutsh/secrets")
        );
        assert_eq!(
            secrets_dir_from(None, "/home/u"),
            PathBuf::from("/home/u/.local/state/nutsh/secrets")
        );
        assert_eq!(
            cache_dir_from(p("/st"), "/home/u"),
            PathBuf::from("/st/nutsh/cache")
        );
        assert_eq!(
            cache_dir_from(None, "/home/u"),
            PathBuf::from("/home/u/.local/state/nutsh/cache")
        );
        assert_eq!(
            logs_dir_from(p("/st"), "/home/u"),
            PathBuf::from("/st/nutsh/logs")
        );
        assert_eq!(
            logs_dir_from(None, "/home/u"),
            PathBuf::from("/home/u/.local/state/nutsh/logs")
        );
    }
}
