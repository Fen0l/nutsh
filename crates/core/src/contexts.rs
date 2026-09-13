//! What the TUI needs from the config crate, without depending on it: rows to show, a way
//! to connect, a way to remove. The binary implements it; tests use an in-memory fake.

use futures::future::BoxFuture;

use crate::session::Session;

/// Re-exported rather than mirrored: `set_header` takes it, the TUI draws by it, and the config
/// file is where it is written down. A second copy of a three-valued setting is a second
/// spelling of it.
///
/// `Interval`, `Refresh` and `Word` ride along for the same reason: the TUI does not depend on
/// the config crate, and it reads a schedule on every frame.
pub use nutsh_config::{Header, Interval, LogLevel, Refresh, Word};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRow {
    /// `(env)` for the `--host` / `NUTSH_HOST` target, which is never saved.
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub cluster: Option<String>,
    pub readonly: bool,
    pub insecure: bool,
    /// `current_context`, or the row the command line selected.
    pub current: bool,
    /// A stored secret exists, or `NUTSH_PASSWORD` was set.
    pub has_password: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct NewContext {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub cluster: Option<String>,
    pub ca_bundle: Option<std::path::PathBuf>,
    pub insecure: bool,
    pub readonly: bool,
}

/// By hand: a derived `Debug` would print the password into whatever log or panic message
/// formatted the value.
impl std::fmt::Debug for NewContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewContext")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"***")
            .field("cluster", &self.cluster)
            .field("ca_bundle", &self.ca_bundle)
            .field("insecure", &self.insecure)
            .field("readonly", &self.readonly)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ConnectRequest {
    /// Stored password only.
    Stored { name: String },
    /// Verify, store, connect.
    Login { name: String, password: String },
    /// Verify, save the context, store the password, connect.
    Add(NewContext),
    /// The `(env)` row; `None` means the scrubbed `NUTSH_PASSWORD`.
    Env { password: Option<String> },
}

/// By hand, for the same reason as [`NewContext`]: the password never reaches the output,
/// only whether there is one.
impl std::fmt::Debug for ConnectRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectRequest::Stored { name } => {
                f.debug_struct("Stored").field("name", name).finish()
            }
            ConnectRequest::Login { name, .. } => f
                .debug_struct("Login")
                .field("name", name)
                .field("password", &"***")
                .finish(),
            ConnectRequest::Add(new) => f.debug_tuple("Add").field(new).finish(),
            ConnectRequest::Env { password } => f
                .debug_struct("Env")
                .field("password", &password.as_ref().map(|_| "***"))
                .finish(),
        }
    }
}

/// A session, and whatever went wrong on the way that was not worth refusing it: a password
/// the store would not take, a config file that could not be written. The connection is real
/// either way, so these are shown next to it rather than raised instead of it.
#[derive(Debug)]
pub struct Connected {
    pub session: Session,
    pub warnings: Vec<String>,
    /// Where the incoming context's cache lives, or `None` for a run with no cache. The
    /// directory is read *before* the connection - that is what lets it adopt pins instead of
    /// negotiating - so what came back rides here with the session that adopted it.
    pub cache_dir: Option<std::path::PathBuf>,
    pub restored: Option<crate::cache::Restored>,
}

pub trait Contexts: Send + Sync {
    /// Re-reads the config file every call. A file that cannot be read is an error to put on
    /// the screen, not an empty list: "no contexts" and "your config file is broken" are
    /// different answers and only one of them is fixed by adding a context.
    fn list(&self) -> anyhow::Result<Vec<ContextRow>>;
    fn connect(&self, req: ConnectRequest) -> BoxFuture<'static, anyhow::Result<Connected>>;
    /// Point the `current` marker at `name`; `None` selects the environment target, the
    /// `(env)` row, whose session has no context name to give.
    ///
    /// An implementation whose rows carry no marker of their own - the test fakes - has
    /// nothing to move, so this does nothing by default.
    fn select(&self, name: Option<&str>) {
        let _ = name;
    }
    /// The context and its secret; refuses `(env)`. The context is gone once this returns
    /// `Ok`, so a secret store that could not be reached comes back as a warning.
    fn remove(&self, name: &str) -> anyhow::Result<Vec<String>>;
    /// Persist the skin `:skin` chose: the current context's own `skin` when it pins one, since
    /// that would override `[skin].name` again at the next start, and `[skin].name` otherwise.
    /// The default does nothing: an implementation with no config file - the test fakes - has
    /// nowhere to write, and `:skin` still applies live either way.
    fn set_skin(&self, name: &str) -> anyhow::Result<()> {
        let _ = name;
        Ok(())
    }
    /// Persist `:mouse`. The default does nothing: an implementation with no config file - the
    /// test fakes - has nowhere to write, and the toggle still applies live either way.
    fn set_mouse(&self, enabled: bool) -> anyhow::Result<()> {
        let _ = enabled;
        Ok(())
    }
    /// Persist `:header`. The default does nothing, for the reason `set_mouse`'s does: the
    /// header is already folded or unfolded on the frame, and a fake with no file to write has
    /// nothing to disagree with.
    fn set_header(&self, header: Header) -> anyhow::Result<()> {
        let _ = header;
        Ok(())
    }
    /// Persist `:log`. The default does nothing, for the reason `set_mouse`'s does: the level
    /// has already moved for this session - `crate::log::set` did that - and a fake with no
    /// file to write has nothing to disagree with.
    fn set_log(&self, level: LogLevel) -> anyhow::Result<()> {
        let _ = level;
        Ok(())
    }
    /// The `[nav] hide` list. The default is empty: an implementation with no config file - the
    /// test fakes - has nothing to read, and `:all` still works either way.
    fn nav_hidden(&self) -> Vec<String> {
        Vec::new()
    }
    /// `[nav] hide_unserved`: whether a kind whose namespace this Prism Central does not serve
    /// is dropped from the menu rather than greyed. **False by default**, and the default is the
    /// whole point - nothing is hidden unless the user asked for it.
    ///
    /// A second accessor rather than one that hands back the whole `[nav]` section: this is the
    /// rule input the menu needs at connect, a bool with no provenance, while a settings screen
    /// wants a display shape nothing but the screen should walk.
    fn nav_hide_unserved(&self) -> bool {
        false
    }
    /// Persist the whole `[nav] hide` list, which is what `:hide` and `:show` recompute. The
    /// default does nothing, for the reason `set_skin`'s does: nowhere to write.
    fn set_nav_hidden(&self, hide: &[String]) -> anyhow::Result<()> {
        let _ = hide;
        Ok(())
    }
    /// Every setting this build has, with where its current value came from. Empty by default:
    /// an implementation with no config file has nothing to show, and the screen says so rather
    /// than drawing rows no key could move.
    fn settings(&self) -> Vec<Setting> {
        Vec::new()
    }
    /// Write one switch back to the config file. The default does nothing, for the reason
    /// `set_skin`'s does: nowhere to write.
    ///
    /// Only the settings that have no command of their own come through here. `skin`, `mouse`
    /// and `header` keep the writers `:skin`, `:mouse` and `:header` already use, so no setting
    /// has two.
    fn set_flag(&self, id: SettingId, on: bool) -> anyhow::Result<()> {
        let _ = (id, on);
        Ok(())
    }
    /// The config file's `[refresh]`. Default empty, for the reason `nav_hidden`'s is: an
    /// implementation with no config file has nothing to read.
    fn refresh(&self) -> nutsh_config::Refresh {
        nutsh_config::Refresh::default()
    }
    /// Write one schedule: `None` is `[refresh] default`, `Some(kind_id)` is one entry of
    /// `[refresh.kinds]`. Through `nutsh_config::save`, the atomic 0600 whole-file rewrite
    /// `set_skin`, `set_nav_hidden` and `set_flag` already go through.
    fn set_interval(
        &self,
        kind: Option<&str>,
        every: nutsh_config::Interval,
    ) -> anyhow::Result<()> {
        let _ = (kind, every);
        Ok(())
    }
}

/// One line of the settings screen: what it is, what it is now, and where that came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub id: SettingId,
    /// Rendered, not typed: `on`, `off`, a skin name, `3 rules`. A `bool` here would make every
    /// drawer spell `on`/`off` for itself, and the screen draws values.
    pub value: String,
    pub source: Source,
    /// `None` when a keystroke can change it; `Some(reason)` when the screen can only show it.
    pub fixed: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingId {
    Skin,
    Cache,
    Mouse,
    Header,
    /// How much this run writes down, and where.
    Log,
    Readonly,
    Guardrails,
    HideUnserved,
    /// `[refresh] default`: how often every kind polls where none says otherwise.
    Refresh,
    /// The current view's kind. Both spell their label `"refresh"`, because that is the name of
    /// the *setting*; the screen builds the per-kind row's own label from the kind's display
    /// name, because that is the name of the *thing*, and the two are not the same string even
    /// though one contains the other.
    RefreshKind,
}

impl SettingId {
    /// What the screen draws and what a message calls it. Here, beside the seam, so the rows
    /// and the messages cannot spell one setting two ways.
    pub fn label(self) -> &'static str {
        match self {
            SettingId::Skin => "skin",
            SettingId::Cache => "cache",
            SettingId::Mouse => "mouse",
            SettingId::Header => "header",
            SettingId::Log => "log",
            SettingId::Readonly => "read-only",
            SettingId::Guardrails => "guardrails",
            SettingId::HideUnserved => "hide unserved",
            SettingId::Refresh | SettingId::RefreshKind => "refresh",
        }
    }
}

/// Where a value came from, in the order a run resolves them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    File,
    Context(String),
    Flag(&'static str),
    /// The catalog's own `poll_secs`. Not `Default`: for an interval the floor-level default is
    /// a curated number, and a row that said `default` over the catalog's three seconds would
    /// send a reader to the config file to look for it.
    Catalog,
    /// `ctrl-t`, this run only. A row that said `default` over a value the user set ten seconds
    /// ago is the exact lie this screen exists to stop.
    Session,
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Default => "default".to_string(),
            Source::File => "config file".to_string(),
            Source::Context(name) => format!("context {name}"),
            Source::Flag(flag) => (*flag).to_string(),
            Source::Catalog => "catalog".to_string(),
            Source::Session => "this session".to_string(),
        }
    }
}

/// What the command line said about this run: the provenance no config file carries, and the
/// reason [`Contexts::settings`] is the binary's to answer rather than the TUI's to guess.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub readonly: bool,
    pub no_cache: bool,
    /// The context this session is on, when it has one: a context that pins `skin` or
    /// `readonly` is the source, not the file's own section.
    pub context: Option<String>,
}

/// The screen's rows, built from the file and the flags. One function, so the binary and the
/// tests agree about precedence and it is written down exactly once.
///
/// **A parsed `Config` cannot say whether a key was present**, so a value equal to the default
/// is reported as `default`. The only file this misreports is one that spells the default out,
/// and about the run itself the two say the same thing.
pub fn settings_of(cfg: &nutsh_config::Config, flags: &Flags) -> Vec<Setting> {
    let ctx = flags
        .context
        .as_ref()
        .and_then(|n| cfg.contexts.get(n).map(|c| (n.clone(), c)));
    let on = |b: bool| if b { "on" } else { "off" }.to_string();
    let from_file = |b: bool| if b { Source::File } else { Source::Default };
    let mut out = Vec::new();

    // The skin the *file* holds; the screen overwrites the value with the skin actually
    // applied, which only the TUI knows. The source is this function's answer either way.
    let ctx_skin = ctx.as_ref().filter(|(_, c)| c.skin.is_some());
    out.push(Setting {
        id: SettingId::Skin,
        value: ctx_skin
            .and_then(|(_, c)| c.skin.clone())
            .or_else(|| cfg.skin.name.clone())
            .unwrap_or_else(|| "default".to_string()),
        source: match (ctx_skin.map(|(n, _)| n.clone()), &cfg.skin.name) {
            (Some(name), _) => Source::Context(name),
            (None, Some(_)) => Source::File,
            (None, None) => Source::Default,
        },
        fixed: None,
    });

    out.push(Setting {
        id: SettingId::Cache,
        value: on(cfg.cache && !flags.no_cache),
        source: match (flags.no_cache, cfg.cache) {
            (true, _) => Source::Flag("--no-cache"),
            (false, true) => Source::Default,
            (false, false) => Source::File,
        },
        fixed: flags
            .no_cache
            .then(|| "--no-cache wins for this run".to_string()),
    });

    // `mouse` and `header` default to the value a file that never mentioned them gets, so the
    // source is the file exactly when the value is not that one.
    out.push(Setting {
        id: SettingId::Mouse,
        value: on(cfg.mouse),
        source: from_file(!cfg.mouse),
        fixed: None,
    });

    out.push(Setting {
        id: SettingId::Header,
        value: match cfg.header {
            Header::Auto => "auto",
            Header::Compact => "compact",
            Header::Full => "full",
        }
        .to_string(),
        source: from_file(!cfg.header.is_auto()),
        fixed: None,
    });

    // The one row this function reads rather than derives. Three things can have moved the
    // level and only one of them is in the file: `NUTSH_LOG` is not, and neither is the `:log`
    // somebody typed a minute ago. `crate::log` is where all three land, so it is asked; a
    // process that installed no subscriber - the fakes, the unit tests - falls back to the file,
    // which is what that process would use if it were the program.
    let live = crate::log::state();
    out.push(Setting {
        id: SettingId::Log,
        value: live
            .as_ref()
            .map_or_else(|| cfg.log.to_string(), |l| l.value()),
        source: live
            .as_ref()
            .map_or_else(|| from_file(!cfg.log.is_off()), |l| l.source.clone()),
        fixed: None,
    });

    let ctx_readonly = ctx.as_ref().filter(|(_, c)| c.readonly);
    out.push(Setting {
        id: SettingId::Readonly,
        value: on(cfg.readonly || flags.readonly || ctx_readonly.is_some()),
        source: match (
            flags.readonly,
            ctx_readonly.map(|(n, _)| n.clone()),
            cfg.readonly,
        ) {
            (true, _, _) => Source::Flag("--readonly"),
            (false, Some(name), _) => Source::Context(name),
            (false, None, true) => Source::File,
            (false, None, false) => Source::Default,
        },
        // Never changed from the screen: `Guardrails` is compiled at connect from the file, the
        // context and the flag, and the TUI holds the compiled object rather than its inputs. A
        // toggle here would change a file and not the session, which is the worst of both.
        fixed: Some(if flags.readonly {
            "--readonly wins for this run".to_string()
        } else {
            "compiled into this session's guardrails at connect".to_string()
        }),
    });

    // Shown because a rule that silently refuses an action is the thing a user hunts for in the
    // wrong file. Not editable here: a rule is a table with five fields, and the screen draws
    // one line per setting.
    out.push(Setting {
        id: SettingId::Guardrails,
        value: match cfg.guardrails.len() {
            0 => "none".to_string(),
            1 => "1 rule".to_string(),
            n => format!("{n} rules"),
        },
        source: from_file(!cfg.guardrails.is_empty()),
        fixed: Some("edit [[guardrails]] in the config file; they compile at connect".to_string()),
    });

    out.push(Setting {
        id: SettingId::HideUnserved,
        value: on(cfg.nav.hide_unserved),
        source: from_file(cfg.nav.hide_unserved),
        fixed: None,
    });
    out
}

pub const ENV_ROW: &str = "(env)";

/// What a context with no stored password says on screen, where the login box is right there
/// to type into. The seam raises it and the Contexts screen shows it, so it is written once.
pub fn no_stored_password(name: &str) -> String {
    format!("no stored password for {name}; run `nutsh ctx login {name}` or type it here")
}

/// The same fact for a run with no screen - `--snapshot`, and anything else that reports on
/// stderr - where "here" is nowhere and the environment is the second way out.
pub fn no_stored_password_cli(name: &str) -> String {
    format!("no stored password for {name}; run `nutsh ctx login {name}` or set NUTSH_PASSWORD")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_context() -> NewContext {
        NewContext {
            name: "lab".into(),
            host: "pc.lab".into(),
            port: 9440,
            username: "admin".into(),
            password: "hunter2".into(),
            cluster: None,
            ca_bundle: None,
            insecure: false,
            readonly: false,
        }
    }

    #[test]
    fn debug_never_prints_a_password() {
        for req in [
            ConnectRequest::Stored { name: "lab".into() },
            ConnectRequest::Login {
                name: "lab".into(),
                password: "hunter2".into(),
            },
            ConnectRequest::Add(new_context()),
            ConnectRequest::Env {
                password: Some("hunter2".into()),
            },
            ConnectRequest::Env { password: None },
        ] {
            let text = format!("{req:?}");
            assert!(!text.contains("hunter2"), "{text}");
        }
        assert!(!format!("{:?}", new_context()).contains("hunter2"));
    }

    /// Both sentences state the same fact and offer `ctx login`; only the second way out
    /// differs, and it has to match where the words are going to be read.
    #[test]
    fn the_missing_password_advice_offers_a_way_out_that_exists() {
        let screen = no_stored_password("lab");
        let cli = no_stored_password_cli("lab");
        for text in [&screen, &cli] {
            assert!(text.starts_with("no stored password for lab"), "{text}");
            assert!(text.contains("`nutsh ctx login lab`"), "{text}");
        }
        assert!(screen.ends_with("or type it here"), "{screen}");
        assert!(cli.ends_with("or set NUTSH_PASSWORD"), "{cli}");
        assert!(!cli.contains("here"), "there is no here on stderr: {cli}");
    }

    /// The `Env` variant still says whether a password came with it, since that is what the
    /// screen shows and what a bug report needs.
    #[test]
    fn debug_shows_whether_env_carries_a_password() {
        assert_eq!(
            format!(
                "{:?}",
                ConnectRequest::Env {
                    password: Some("hunter2".into())
                }
            ),
            "Env { password: Some(\"***\") }"
        );
        assert_eq!(
            format!("{:?}", ConnectRequest::Env { password: None }),
            "Env { password: None }"
        );
    }

    fn value_of(rows: &[Setting], id: SettingId) -> (String, String) {
        let s = rows.iter().find(|s| s.id == id).expect("a row per setting");
        (s.value.clone(), s.source.label())
    }

    /// A file that says nothing says `default` everywhere, and every setting this build has is
    /// on the screen: a setting that is on and not listed is one the user hunts for in the
    /// wrong file.
    #[test]
    fn an_empty_file_reports_every_setting_as_a_default() {
        let rows = settings_of(&nutsh_config::Config::default(), &Flags::default());
        assert_eq!(
            rows.iter().map(|s| s.id).collect::<Vec<_>>(),
            [
                SettingId::Skin,
                SettingId::Cache,
                SettingId::Mouse,
                SettingId::Header,
                SettingId::Log,
                SettingId::Readonly,
                SettingId::Guardrails,
                SettingId::HideUnserved,
            ]
        );
        for row in &rows {
            assert_eq!(row.source, Source::Default, "{:?}", row.id);
        }
        assert_eq!(value_of(&rows, SettingId::Cache).0, "on");
        assert_eq!(value_of(&rows, SettingId::Mouse).0, "on");
        assert_eq!(value_of(&rows, SettingId::Header).0, "auto");
        assert_eq!(value_of(&rows, SettingId::Log).0, "off");
        assert_eq!(value_of(&rows, SettingId::Guardrails).0, "none");
    }

    /// Precedence, which is the whole reason the source column exists: a flag beats the file,
    /// and a context that pins a skin or read-only beats the file's own section.
    #[test]
    fn a_flag_and_a_context_outrank_the_file() {
        let mut cfg = nutsh_config::Config::default();
        cfg.skin.name = Some("gruvbox-dark".into());
        cfg.cache = false;
        cfg.guardrails.push(nutsh_config::RuleSpec::default());
        cfg.contexts.insert(
            "lab".into(),
            nutsh_config::Context {
                host: "pc.lab".into(),
                port: 9440,
                username: "admin".into(),
                verify_tls: true,
                ca_bundle: None,
                cluster: None,
                readonly: true,
                skin: Some("catppuccin-latte".into()),
            },
        );
        let flags = Flags {
            readonly: false,
            no_cache: true,
            context: Some("lab".into()),
        };
        let rows = settings_of(&cfg, &flags);
        assert_eq!(
            value_of(&rows, SettingId::Skin),
            ("catppuccin-latte".into(), "context lab".into())
        );
        assert_eq!(
            value_of(&rows, SettingId::Cache),
            ("off".into(), "--no-cache".into())
        );
        assert_eq!(
            value_of(&rows, SettingId::Readonly),
            ("on".into(), "context lab".into())
        );
        assert_eq!(
            value_of(&rows, SettingId::Guardrails),
            ("1 rule".into(), "config file".into())
        );
        // `--readonly` outranks even the context, and says so.
        let rows = settings_of(
            &cfg,
            &Flags {
                readonly: true,
                ..flags
            },
        );
        assert_eq!(value_of(&rows, SettingId::Readonly).1, "--readonly");
        assert!(
            rows.iter()
                .find(|s| s.id == SettingId::Readonly)
                .and_then(|s| s.fixed.as_deref())
                .is_some_and(|why| why.contains("--readonly wins")),
            "and cannot be changed here"
        );
    }
}
