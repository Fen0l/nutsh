//! The config file's shape.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Prism Central's port, and the one a context omits from the file.
///
/// Twinned in `crates/tui/src/lib.rs`, on purpose: the TUI decides when to hide `:9440` from a
/// row, and it does not depend on this crate - it reaches contexts through the `Contexts` seam
/// instead. Change one and change the other.
pub const DEFAULT_PORT: u16 = 9440;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_context: Option<String>,
    /// Refuse every mutation, whatever the context says. ORed with the context's and `--readonly`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub readonly: bool,
    /// Read and write the cache under `$XDG_STATE_HOME/nutsh/cache`. Declared above
    /// `guardrails`, `skin` and `contexts` for the reason `skin` is, and skipped when true so
    /// `ctx add` never starts writing it into files that never had it. `--no-cache` overrides
    /// it for one run.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub cache: bool,
    /// Mouse capture: rows, menu items, headers and the wheel.
    ///
    /// It sits beside `readonly` and `cache` because `toml::to_string_pretty` emits bare keys
    /// before tables and arrays of tables: a bare key declared after `guardrails`, `skin` or
    /// `contexts` would be serialised beneath the last of them and read back as
    /// `guardrails[n].mouse` or `skin.mouse`.
    ///
    /// Skipped when **true**, not when false, so a file that never mentions the mouse keeps
    /// never mentioning it.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub mouse: bool,
    /// How tall the header is drawn. A bare key, so it sits beside `mouse` and above every
    /// table for the reason `mouse` does, and skipped when it is `auto` so a file that never
    /// mentioned it keeps never mentioning it.
    #[serde(default, skip_serializing_if = "Header::is_auto")]
    pub header: Header,
    /// How much of what this program does is written down. A bare key beside `mouse` and
    /// `header`, above every table for the reason they are, and skipped when it is `off` so a
    /// user who never asked for a log has no key about one. `NUTSH_LOG` overrides it for a run.
    #[serde(default, skip_serializing_if = "LogLevel::is_off")]
    pub log: LogLevel,
    /// Declared before `contexts`, like `skin`, and skipped when empty, so `ctx add` never
    /// starts writing `[[guardrails]]` into files that never had them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guardrails: Vec<RuleSpec>,
    /// Declared before `contexts` so `toml::to_string_pretty` writes `[skin]` above the
    /// `[contexts.*]` sections: beneath them it would read as though it belonged to the last
    /// context.
    #[serde(default, skip_serializing_if = "Skin::is_default")]
    pub skin: Skin,
    /// What the menu never shows. Declared among the tables and before `contexts`, which is the
    /// mirror image of where `mouse` had to go: a *bare* key must sit above every table or
    /// `to_string_pretty` writes it beneath the last one and it reads back as that table's
    /// member, while a *table* must sit below every bare key for the same reason, and above
    /// `contexts` so `[nav]` is never written into the middle of the `[contexts.*]` run where a
    /// reader would take it for one context's business.
    ///
    /// Skipped when default, like `skin` and `guardrails`, so `ctx add` - which rewrites the
    /// whole file - never starts writing a `[nav]` section into files that never had one.
    #[serde(default, skip_serializing_if = "Nav::is_default")]
    pub nav: Nav,
    /// How often a kind's views poll. A table, so it sits below every bare key and above
    /// `contexts`, for the reasons `nav` gives; skipped when default so `ctx add` never starts
    /// writing a `[refresh]` into files that never had one.
    #[serde(default, skip_serializing_if = "Refresh::is_default")]
    pub refresh: Refresh,
    #[serde(default)]
    pub contexts: BTreeMap<String, Context>,
}

/// Hand-written, because a derived one would make `cache` and `mouse` **false** - and
/// `Config::default()` is exactly what the TUI falls back to when the file cannot be read, and
/// what `file::load` returns for a file that is not there, so both would be off in the one case
/// nobody asked for it: a first run, whose next `ctx add` would then write the `false` down.
impl Default for Config {
    fn default() -> Config {
        Config {
            current_context: None,
            readonly: false,
            cache: true,
            mouse: true,
            header: Header::Auto,
            log: LogLevel::Off,
            guardrails: Vec::new(),
            skin: Skin::default(),
            nav: Nav::default(),
            refresh: Refresh::default(),
            contexts: BTreeMap::new(),
        }
    }
}

/// How tall the header is drawn: the full box with its hint grid and stats block, the one line
/// a short terminal gets instead, or by the size of the terminal.
///
/// `Auto` is the answer for almost everybody, which is why it is the default: the two settings
/// exist for a terminal whose height says one thing and whose user wants the other - a tall
/// window kept for the rows, a short one where the counters matter more than a row of table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Header {
    /// Full on a frame tall enough for it, folded below.
    #[default]
    Auto,
    /// Always the one line.
    Compact,
    /// Always the box.
    Full,
}

impl Header {
    /// Whether this is what a file that never mentioned the key gets. Named for the serde
    /// attribute that reads it, as `Skin::is_default` and `Nav::is_default` are.
    pub fn is_auto(&self) -> bool {
        *self == Header::Auto
    }
}

/// How much a run writes down: the `tracing` levels, plus the `off` that is the default.
///
/// Off is the whole point of the default. A user who never asks for a log gets the behaviour
/// this program had before there was one, and no file on disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    #[default]
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Named for the serde attribute that reads it, as `Header::is_auto` is.
    pub fn is_off(&self) -> bool {
        *self == LogLevel::Off
    }

    /// The spelling the config file, `NUTSH_LOG` and the status line all use. One function, so
    /// the six words are written down once.
    pub fn word(self) -> &'static str {
        match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    /// One of the six words, case-insensitively, or `None` for anything else - which is what
    /// lets `NUTSH_LOG` take a level word and a `tracing_subscriber` filter string without
    /// either having to be marked as which.
    pub fn parse(word: &str) -> Option<LogLevel> {
        [
            LogLevel::Off,
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ]
        .into_iter()
        .find(|l| l.word().eq_ignore_ascii_case(word))
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.word())
    }
}

/// The `[nav]` section: what the menu never shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nav {
    /// Drop the kinds whose namespace this Prism Central does not serve, instead of greying
    /// them. **False unless the user sets it**: nothing is hidden unless it was asked for.
    ///
    /// Declared before `hide` - both are bare keys, so neither can swallow the other, and a
    /// reader who meets the switch first knows the list below it is the whole of what this menu
    /// drops.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hide_unserved: bool,
    /// Kind ids, page ids, or group names never to show. Ids match exactly; a group name matches
    /// case-insensitively, because it is display text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hide: Vec<String>,
}

impl Nav {
    /// Written, not derived: `Skin::is_default` is the precedent, and `skip_serializing_if` takes
    /// a path to a predicate, not `Default`.
    pub fn is_default(&self) -> bool {
        !self.hide_unserved && self.hide.is_empty()
    }
}

/// The `[refresh]` section: how often a kind's views poll, where the catalog's `poll_secs` is
/// not what this user wants.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refresh {
    /// Where no kind says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Interval>,
    /// By catalog kind id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kinds: BTreeMap<String, Interval>,
}

impl Refresh {
    /// Written, not derived, for the reason `Nav::is_default` and `Skin::is_default` are:
    /// `skip_serializing_if` takes a path to a predicate, not `Default`.
    pub fn is_default(&self) -> bool {
        self.default.is_none() && self.kinds.is_empty()
    }
}

/// What a schedule says: a number of seconds, the catalog's own `poll_secs` (`"auto"`), or no
/// schedule at all (`"off"`).
///
/// `untagged`, so the file reads `default = 60` and `default = "off"` rather than making a
/// person spell a tag for a scalar. Seconds and not a `"30s"` string, because a config file
/// should carry a number where it means a number; `nutsh_core::refresh::parse` is what turns the
/// `1m` a person types at the palette into the `60` that is written here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Interval {
    Secs(u32),
    Word(Word),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Word {
    Auto,
    Off,
}

/// The `[skin]` section. This crate stores strings the way it stores a host it cannot ping:
/// the binary validates the name with `nutsh_tui::theme::validate_skin` and warns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Fill every view with the skin's own `base` instead of leaving the terminal's
    /// background - possibly transparent - alone.
    #[serde(default, skip_serializing_if = "is_false")]
    pub background: bool,
    /// Per-swatch overrides; keys are `Palette` field names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, String>,
}

impl Skin {
    pub fn is_default(&self) -> bool {
        *self == Skin::default()
    }
}

/// One `[[guardrails]]` table. Strings and numbers only, so this crate stays free of the
/// catalog; `core::guardrails::Rule::from_spec` converts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deny: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `"none"` | `"yes"` | `"type-name"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bulk: Option<usize>,
}

/// One Prism Central plus an optional cluster scope. The password is never here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub host: String,
    #[serde(default = "default_port", skip_serializing_if = "is_default_port")]
    pub port: u16,
    pub username: String,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub verify_tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_bundle: Option<PathBuf>,
    /// Cluster name, resolved to an extId at connect time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub readonly: bool,
    /// A skin for this context alone, overriding `[skin].name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
}

impl Context {
    /// Account under which this context's password is stored.
    pub fn secret_key(&self) -> String {
        secret_key(&self.username, &self.host, self.port)
    }
}

/// Account under which a password is stored: `user@host:port`. Callers that let flags override
/// a context's username or port must key off the effective values, not the stored ones.
pub fn secret_key(username: &str, host: &str, port: u16) -> String {
    format!("{username}@{host}:{port}")
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn default_port() -> u16 {
    DEFAULT_PORT
}
fn is_default_port(p: &u16) -> bool {
    *p == DEFAULT_PORT
}
fn default_true() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cache` is declared above `guardrails`, `skin` and `contexts` for the reason `skin` is:
    /// `toml::to_string_pretty` writes a scalar after a table *into* that table, so a `cache`
    /// key beneath `[contexts.lab]` would read as that context's and parse as an unknown key.
    /// And it is skipped when true, so `ctx add` never starts writing it into files that never
    /// had it.
    #[test]
    fn cache_defaults_to_true_is_not_written_and_sits_above_the_contexts() {
        let cfg: Config =
            toml::from_str("[contexts.lab]\nhost = \"h\"\nusername = \"u\"\n").unwrap();
        assert!(cfg.cache, "absent means on");
        let written = toml::to_string_pretty(&cfg).unwrap();
        assert!(!written.contains("cache"), "{written}");
        let off: Config = toml::from_str("cache = false\n").unwrap();
        assert!(!off.cache);
        let written = toml::to_string_pretty(&off).unwrap();
        assert!(written.contains("cache = false"), "{written}");
        let mut with_ctx = off.clone();
        with_ctx.contexts.insert(
            "lab".into(),
            Context {
                host: "h".into(),
                port: 9440,
                username: "u".into(),
                verify_tls: true,
                ca_bundle: None,
                cluster: None,
                readonly: false,
                skin: None,
            },
        );
        let written = toml::to_string_pretty(&with_ctx).unwrap();
        assert!(
            written.find("cache = false").unwrap() < written.find("[contexts.lab]").unwrap(),
            "{written}"
        );
        assert_eq!(toml::from_str::<Config>(&written).unwrap(), with_ctx);
    }

    /// `[refresh]` is absent until something is in it, round-trips all three spellings of an
    /// interval, and is written above the `[contexts.*]` tables - beneath them a reader would
    /// take it for the last context's business.
    #[test]
    fn refresh_is_absent_until_used_and_sits_above_the_contexts() {
        let cfg: Config =
            toml::from_str("[contexts.lab]\nhost = \"h\"\nusername = \"u\"\n").unwrap();
        assert!(cfg.refresh.is_default());
        let written = toml::to_string_pretty(&cfg).unwrap();
        assert!(!written.contains("refresh"), "{written}");

        let mut full: Config = toml::from_str(
            "[refresh]\ndefault = 60\n\n[refresh.kinds]\n\"prism.config.Task\" = \"auto\"\n\"vmm.ahv.config.Vm\" = \"off\"\n",
        )
        .unwrap();
        assert_eq!(full.refresh.default, Some(Interval::Secs(60)));
        assert_eq!(
            full.refresh.kinds["prism.config.Task"],
            Interval::Word(Word::Auto)
        );
        assert_eq!(
            full.refresh.kinds["vmm.ahv.config.Vm"],
            Interval::Word(Word::Off)
        );
        full.contexts.insert(
            "lab".into(),
            Context {
                host: "h".into(),
                port: 9440,
                username: "u".into(),
                verify_tls: true,
                ca_bundle: None,
                cluster: None,
                readonly: false,
                skin: None,
            },
        );
        let written = toml::to_string_pretty(&full).unwrap();
        assert!(
            written.find("[refresh]").unwrap() < written.find("[contexts.lab]").unwrap(),
            "{written}"
        );
        assert_eq!(toml::from_str::<Config>(&written).unwrap(), full);
    }

    /// `auto` is what a file that never mentions the key gets, and what a file that mentions
    /// it keeps mentioning; the two overrides round-trip through their lower-case spellings and
    /// land above the tables, where a bare key has to be.
    #[test]
    fn the_header_defaults_to_auto_is_skipped_when_auto_and_sits_above_the_tables() {
        assert_eq!(Config::default().header, Header::Auto);
        let bare = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(
            !bare.contains("header"),
            "a file that never mentions it keeps not: {bare}"
        );
        for (text, expect) in [
            ("header = \"compact\"\n", Header::Compact),
            ("header = \"full\"\n", Header::Full),
            ("header = \"auto\"\n", Header::Auto),
        ] {
            let cfg: Config = toml::from_str(text).expect(text);
            assert_eq!(cfg.header, expect, "{text}");
        }
        let mut cfg: Config = toml::from_str("header = \"compact\"\n").unwrap();
        cfg.contexts.insert(
            "lab".into(),
            Context {
                host: "h".into(),
                port: 9440,
                username: "u".into(),
                verify_tls: true,
                ca_bundle: None,
                cluster: None,
                readonly: false,
                skin: None,
            },
        );
        let written = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            written.find("header").unwrap() < written.find("[contexts.lab]").unwrap(),
            "a bare key beneath a table reads as that table's: {written}"
        );
        assert_eq!(toml::from_str::<Config>(&written).unwrap(), cfg);
        // A spelling the enum does not have is a refusal, not a silent `auto`.
        assert!(toml::from_str::<Config>("header = \"small\"\n").is_err());
    }

    /// Off is what a file that never mentions the key gets, and a file that never mentions it
    /// keeps never mentioning it: the whole promise of the default is that a user who never
    /// asked for a log has neither a key about one nor a file on disk.
    #[test]
    fn the_log_defaults_to_off_is_skipped_when_off_and_sits_above_the_tables() {
        assert_eq!(Config::default().log, LogLevel::Off);
        let bare = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!bare.contains("log"), "{bare}");
        for (text, expect) in [
            ("log = \"off\"\n", LogLevel::Off),
            ("log = \"error\"\n", LogLevel::Error),
            ("log = \"warn\"\n", LogLevel::Warn),
            ("log = \"info\"\n", LogLevel::Info),
            ("log = \"debug\"\n", LogLevel::Debug),
            ("log = \"trace\"\n", LogLevel::Trace),
        ] {
            let cfg: Config = toml::from_str(text).expect(text);
            assert_eq!(cfg.log, expect, "{text}");
            assert_eq!(LogLevel::parse(expect.word()), Some(expect));
        }
        // A spelling the enum does not have is a refusal, not a silent `off`.
        assert!(toml::from_str::<Config>("log = \"verbose\"\n").is_err());
        assert_eq!(LogLevel::parse("nutsh_prism=debug"), None);

        let mut cfg: Config = toml::from_str("log = \"debug\"\n").unwrap();
        cfg.contexts.insert(
            "lab".into(),
            Context {
                host: "h".into(),
                port: 9440,
                username: "u".into(),
                verify_tls: true,
                ca_bundle: None,
                cluster: None,
                readonly: false,
                skin: None,
            },
        );
        let written = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            written.find("log").unwrap() < written.find("[contexts.lab]").unwrap(),
            "a bare key beneath a table reads as that table's: {written}"
        );
        assert_eq!(toml::from_str::<Config>(&written).unwrap(), cfg);
    }

    /// `deny_unknown_fields` means the field, the `Default` impl and any config file that uses
    /// the key have to land together, and a round trip through `file::save` is the test.
    #[test]
    fn the_mouse_defaults_on_is_skipped_when_on_and_sits_above_the_tables() {
        assert!(Config::default().mouse, "a first run has the mouse");
        let bare = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(
            !bare.contains("mouse"),
            "a file that never mentions it keeps not: {bare}"
        );

        let text = "current_context = \"lab\"\nmouse = false\n\n\
                    [[guardrails]]\nconfirm = \"yes\"\n\n\
                    [skin]\nname = \"nord\"\n\n\
                    [contexts.lab]\nhost = \"pc.lab.example\"\nusername = \"admin\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(!cfg.mouse);
        let written = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(
            toml::from_str::<Config>(&written).unwrap(),
            cfg,
            "round trip"
        );
        for table in ["[[guardrails]]", "[skin]", "[contexts.lab]"] {
            assert!(
                written.find("mouse").unwrap() < written.find(table).unwrap(),
                "mouse must be above {table}: {written}"
            );
        }

        // A file that says nothing gets the default, not `false`.
        let quiet: Config = toml::from_str("readonly = true\n").unwrap();
        assert!(quiet.mouse);
    }

    /// `save` rewrites the whole file, so a `[[guardrails]]` table that did not survive a
    /// round trip would be one `ctx add` away from silently vanishing; and it is written above
    /// the contexts, where beneath them it would read as though it belonged to the last one.
    #[test]
    fn guardrails_round_trip_above_the_contexts_and_defaults_are_not_written() {
        let text = "current_context = \"lab\"\nreadonly = true\n\n\
                    [[guardrails]]\ncontexts = [\"*prod*\"]\nactions = [\"delete\"]\n\
                    deny = true\nreason = \"no\"\n\n\
                    [[guardrails]]\nconfirm = \"type-name\"\nmax_bulk = 1\n\n\
                    [contexts.lab]\nhost = \"pc.lab.example\"\nusername = \"admin\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.readonly);
        assert_eq!(cfg.guardrails.len(), 2);
        assert_eq!(cfg.guardrails[1].confirm.as_deref(), Some("type-name"));
        let written = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(toml::from_str::<Config>(&written).unwrap(), cfg);
        assert!(
            written.find("[[guardrails]]").unwrap() < written.find("[contexts.lab]").unwrap(),
            "{written}"
        );
        // Absent lists and false flags are not written, so a rule reads as it was typed.
        assert!(!written.contains("kinds"), "{written}");
        assert!(!written.contains("deny = false"), "{written}");
        // A config with neither does not grow either key.
        let bare = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!bare.contains("readonly"), "{bare}");
        assert!(!bare.contains("guardrails"), "{bare}");
    }

    /// `[nav]` is declared before `contexts` and skipped when default, for the reason `skin` and
    /// `guardrails` are: `ctx add` rewrites the whole file, and it must never start writing a
    /// `[nav]` section into files that never had one.
    #[test]
    fn nav_round_trips_above_the_contexts_and_is_not_written_when_empty() {
        let text = "[nav]\nhide_unserved = true\n\
                    hide = [\"vmm.esxi.config.Vm\", \"Data Protection\"]\n\n\
                    [contexts.lab]\nhost = \"pc.lab.example\"\nusername = \"admin\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.nav.hide, ["vmm.esxi.config.Vm", "Data Protection"]);
        assert!(cfg.nav.hide_unserved);
        // Off unless the file says otherwise: nothing is hidden unless the user asked for it.
        assert!(!Config::default().nav.hide_unserved);
        let written = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(toml::from_str::<Config>(&written).unwrap(), cfg);
        assert!(
            written.find("[nav]").unwrap() < written.find("[contexts.lab]").unwrap(),
            "{written}"
        );
        let bare = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!bare.contains("nav"), "{bare}");
        // Through the crate root, which is the path a consumer has: `Config` carries a `Nav`,
        // so a caller that wants to name the type must be able to reach it without knowing
        // which module it is declared in.
        assert!(crate::Nav::default().is_default());
    }
}
