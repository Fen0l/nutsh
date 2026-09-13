//! Load and save the config file; resolve which context to use.

use std::path::Path;

use crate::error::ConfigError;
use crate::model::{Config, Context, valid_name};

/// A missing file is an empty config. Unknown keys, a `password` key and invalid names are
/// errors.
///
/// A `current_context` naming no context is not: the one screen that exists to fix it - the
/// Contexts screen, and `ctx list` and `ctx use` behind it - has to be able to read the file
/// to show the contexts that are still there. Selecting that name is what fails, in
/// [`Config::resolve`], which is where the user finds out.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    // A password is looked for before the parser is allowed to complain: a `password` key that
    // is *also* a syntax error - an unterminated `password = "hunter2` - would otherwise be
    // reported by quoting the line it sits on, which is the password.
    let table: toml::Table = match toml::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            return Err(password_key(path, &text).unwrap_or_else(|| parse_error(path, &text, &e)));
        }
    };
    if let Some(e) = password_in(path, &table) {
        return Err(e);
    }
    let cfg: Config = toml::from_str(&text).map_err(|e| parse_error(path, &text, &e))?;
    for name in cfg.contexts.keys() {
        if !valid_name(name) {
            return Err(ConfigError::InvalidName(name.clone()));
        }
    }
    Ok(cfg)
}

/// What the parser said, without what it was reading: `toml`'s own `Display` quotes the
/// offending source line under a caret, and that line can be a password. Only the parser's
/// message and the line number cross this boundary; the file's text never does.
fn parse_error(path: &Path, text: &str, e: &toml::de::Error) -> ConfigError {
    let message = match e.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_string(),
    };
    ConfigError::Parse {
        path: path.to_path_buf(),
        message,
    }
}

/// A `password` key in a file that parsed: inside a context, or loose at the top level.
fn password_in(path: &Path, table: &toml::Table) -> Option<ConfigError> {
    if table.contains_key("password") {
        return Some(ConfigError::PasswordKey {
            path: path.to_path_buf(),
        });
    }
    table
        .get("contexts")?
        .as_table()?
        .iter()
        .find(|(_, ctx)| ctx.get("password").is_some())
        .map(|(name, _)| ConfigError::PasswordInFile {
            path: path.to_path_buf(),
            name: name.clone(),
        })
}

/// The same rule by eye, for a file the parser would not read at all. The error the parser
/// wants to report may sit on the password line itself, so a `password = …` key is recognised
/// by shape instead. Mistaking a line for one costs nothing: the file is already broken.
fn password_key(path: &Path, text: &str) -> Option<ConfigError> {
    let mut section: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = header
                .trim()
                .strip_prefix("contexts.")
                .map(|name| name.trim().trim_matches('"').to_string());
        } else if line
            .strip_prefix("password")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
        {
            let path = path.to_path_buf();
            return Some(match section {
                Some(name) => ConfigError::PasswordInFile { path, name },
                None => ConfigError::PasswordKey { path },
            });
        }
    }
    None
}

/// Write atomically with mode 0600, rewriting the whole file, through
/// [`crate::atomic::write_private`]: a directory this call has to create is created 0700, an
/// existing one is left as the user set it, and the payload goes through a uniquely named
/// `create_new` temp file so the mode is never a pre-planted file's.
pub fn save(path: &Path, cfg: &Config) -> Result<(), ConfigError> {
    let text = toml::to_string_pretty(cfg).map_err(|e| ConfigError::Serialize(e.to_string()))?;
    crate::atomic::write_private(path, &text).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })
}

impl Config {
    pub fn get(&self, name: &str) -> Result<&Context, ConfigError> {
        self.contexts
            .get(name)
            .ok_or_else(|| ConfigError::UnknownContext {
                name: name.to_string(),
                known: self.known(),
            })
    }

    /// `explicit` (the `--context` flag) beats `env` (`NUTSH_CONTEXT`) beats `current_context`.
    pub fn resolve(
        &self,
        explicit: Option<&str>,
        env: Option<&str>,
    ) -> Result<(&str, &Context), ConfigError> {
        let name = explicit
            .or(env)
            .or(self.current_context.as_deref())
            .ok_or_else(|| ConfigError::NoContext(crate::paths::config_path()))?;
        let (key, ctx) =
            self.contexts
                .get_key_value(name)
                .ok_or_else(|| ConfigError::UnknownContext {
                    name: name.to_string(),
                    known: self.known(),
                })?;
        Ok((key.as_str(), ctx))
    }

    pub fn known(&self) -> String {
        if self.contexts.is_empty() {
            "(none)".to_string()
        } else {
            self.contexts.keys().cloned().collect::<Vec<_>>().join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Context;

    fn ctx(host: &str) -> Context {
        Context {
            host: host.into(),
            port: 9440,
            username: "admin".into(),
            verify_tls: true,
            ca_bundle: None,
            cluster: None,
            readonly: false,
            skin: None,
        }
    }

    #[test]
    fn missing_file_is_an_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn save_then_load_round_trips_and_omits_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let mut cfg = Config::default();
        cfg.contexts.insert(
            "lab".into(),
            Context {
                cluster: Some("prod-01".into()),
                verify_tls: false,
                ..ctx("pc.lab")
            },
        );
        cfg.contexts.insert(
            "prod".into(),
            Context {
                readonly: true,
                ..ctx("pc.prod")
            },
        );
        cfg.current_context = Some("lab".into());
        save(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[contexts.lab]"), "{text}");
        assert!(
            !text.contains("port"),
            "default port must not be written:\n{text}"
        );
        assert!(text.contains("verify_tls = false"));
        assert!(text.contains("readonly = true"));
        assert!(!text.contains("readonly = false"));
        assert_eq!(load(&path).unwrap(), cfg);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    /// A config carrying a skin round-trips, and `[skin]` is written above the contexts: under
    /// them it would read as though it belonged to the last one.
    #[test]
    fn save_round_trips_a_skin_and_writes_it_above_the_contexts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.skin.name = Some("gruvbox-dark".into());
        cfg.skin.background = true;
        cfg.skin.colors.insert("red".into(), "#fb4934".into());
        cfg.contexts.insert(
            "lab".into(),
            Context {
                host: "pc.lab.example".into(),
                port: 9440,
                username: "admin".into(),
                verify_tls: true,
                ca_bundle: None,
                cluster: None,
                readonly: false,
                skin: Some("catppuccin-latte".into()),
            },
        );
        save(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.find("[skin]").unwrap() < text.find("[contexts.lab]").unwrap(),
            "{text}"
        );
        assert_eq!(load(&path).unwrap(), cfg);

        // A config with no skin does not grow an empty section.
        let bare = Config::default();
        save(&path, &bare).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("[skin]"));
    }

    /// A `save` onto a file someone else planted must not inherit that file's mode: the
    /// rename swaps in our own 0600 temp file, and no temp file is left behind.
    #[cfg(unix)]
    #[test]
    fn save_replaces_a_planted_world_readable_target_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "current_context = \"planted\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let mut cfg = Config::default();
        cfg.contexts.insert("lab".into(), ctx("pc.lab"));
        cfg.current_context = Some("lab".into());
        save(&path, &cfg).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load(&path).unwrap(), cfg);
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("config.toml")]);
        // And the temp file is never opened over an existing one, so a planted file cannot
        // survive with its own mode either.
        let planted = dir.path().join(".planted.tmp");
        std::fs::write(&planted, "x").unwrap();
        assert_eq!(
            crate::atomic::write_temp(&planted, "y").unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_does_not_chmod_an_existing_directory() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = existing.join("config.toml");
        save(&path, &Config::default()).unwrap();
        assert_eq!(mode(&existing), 0o755);
        assert_eq!(mode(&path), 0o600);
        // A directory `save` does create is private.
        let fresh = existing.join("fresh");
        save(&fresh.join("config.toml"), &Config::default()).unwrap();
        assert_eq!(mode(&fresh), 0o700);
    }

    #[test]
    fn unknown_key_and_password_key_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[contexts.lab]\nhost = \"h\"\nusername = \"u\"\ncolour = 1\n",
        )
        .unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("colour"), "{err}");
        std::fs::write(
            &path,
            "[contexts.lab]\nhost = \"h\"\nusername = \"u\"\npassword = \"x\"\n",
        )
        .unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("ctx login lab"), "{err}");
    }

    /// A parse error says what and where, never what it read: the line it choked on can be a
    /// password, and this error is printed on stderr and shown in a frame.
    #[test]
    fn a_parse_error_never_quotes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for text in [
            // A password key that is also a syntax error, in a context and outside one.
            "[contexts.lab]\nhost = \"h\"\nusername = \"u\"\npassword = \"hunter2\n",
            "password = \"hunter2\n",
            // And a broken line that is not a password key at all, whose value is still the
            // user's business and not the error message's.
            "current_context = \"hunter2\n",
            "[contexts.lab]\nhost = \"hunter2\ntoken = 1\n",
        ] {
            std::fs::write(&path, text).unwrap();
            let err = load(&path).unwrap_err().to_string();
            assert!(!err.contains("hunter2"), "{text:?} leaked: {err}");
            assert!(err.contains("config.toml"), "{err}");
        }
        // The line number survives, which is what makes the message usable at all.
        std::fs::write(&path, "current_context = \"a\"\nnot = [toml\n").unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
    }

    /// A `password` key the file has no context to blame it on is refused just as loudly, and
    /// without echoing it.
    #[test]
    fn a_password_key_outside_a_context_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for text in [
            "password = \"hunter2\"\n[contexts.lab]\nhost = \"h\"\nusername = \"u\"\n",
            "password = \"hunter2\n",
        ] {
            std::fs::write(&path, text).unwrap();
            let err = load(&path).unwrap_err();
            assert!(
                matches!(err, ConfigError::PasswordKey { .. }),
                "{text:?}: {err:?}"
            );
            assert!(!err.to_string().contains("hunter2"), "{err}");
        }
        // Inside a context it still names the context, whether or not the file parses.
        for text in [
            "[contexts.lab]\nhost = \"h\"\nusername = \"u\"\npassword = \"hunter2\"\n",
            "[contexts.lab]\nhost = \"h\"\nusername = \"u\"\npassword = \"hunter2\n",
        ] {
            std::fs::write(&path, text).unwrap();
            let err = load(&path).unwrap_err().to_string();
            assert!(err.contains("ctx login lab"), "{text:?}: {err}");
            assert!(!err.contains("hunter2"), "{err}");
        }
    }

    #[test]
    fn invalid_names_and_dangling_current_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[contexts.\"bad name\"]\nhost = \"h\"\nusername = \"u\"\n",
        )
        .unwrap();
        assert!(
            load(&path)
                .unwrap_err()
                .to_string()
                .contains("invalid context name")
        );
    }

    /// A `current_context` that names nothing does not cost the user the rest of the file: the
    /// contexts still load, so `ctx list` and the Contexts screen can show them and `ctx use`
    /// can point the pointer at one. Only asking for that context fails.
    #[test]
    fn a_dangling_current_context_loads_and_fails_when_it_is_selected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "current_context = \"nope\"\n[contexts.lab]\nhost = \"h\"\nusername = \"u\"\n",
        )
        .unwrap();
        let cfg = load(&path).expect("the contexts still load");
        assert!(cfg.contexts.contains_key("lab"));
        let e = cfg.resolve(None, None).unwrap_err();
        assert!(e.to_string().contains("unknown context nope"), "{e}");
        // Naming one that is there still works, which is what makes the file repairable.
        assert_eq!(cfg.resolve(Some("lab"), None).unwrap().0, "lab");
    }

    #[test]
    fn resolution_prefers_explicit_then_env_then_current() {
        let mut cfg = Config::default();
        cfg.contexts.insert("a".into(), ctx("a"));
        cfg.contexts.insert("b".into(), ctx("b"));
        cfg.current_context = Some("a".into());
        assert_eq!(cfg.resolve(Some("b"), None).unwrap().0, "b");
        assert_eq!(cfg.resolve(None, Some("b")).unwrap().0, "b");
        assert_eq!(cfg.resolve(None, None).unwrap().0, "a");
        assert!(
            cfg.resolve(Some("zzz"), None)
                .unwrap_err()
                .to_string()
                .contains("unknown context zzz")
        );
        cfg.current_context = None;
        assert!(matches!(
            cfg.resolve(None, None),
            Err(ConfigError::NoContext(_))
        ));
    }
}
