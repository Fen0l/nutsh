//! The binary's `Contexts`: config file plus secret store plus the `(env)` target.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use nutsh_config::{Config, Context, Secrets, valid_name};
use nutsh_core::contexts::{
    ConnectRequest, Connected, ContextRow, Contexts, ENV_ROW, Flags, NewContext, Setting,
    SettingId, no_stored_password,
};
use nutsh_core::session::{Scope, Session};
use nutsh_prism::Profile;

use crate::ConnArgs;

/// What the command line said about the connection, over what a context stores. These apply
/// to *every* row, exactly as they apply to the single context `--check` resolves: `-u other`
/// means this run is other's session, so other's password is the one looked up and stored,
/// and `--insecure` turns verification off wherever the user goes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Overrides {
    pub(crate) username: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) insecure: bool,
    pub(crate) ca_bundle: Option<PathBuf>,
    /// `--plain-http`, for tests against the mock; refused for a non-loopback host.
    pub(crate) plain_http: bool,
    /// `--readonly`. It applies to every row for the same reason `--insecure` does: it is a
    /// property of this run, so a session opened later from the Contexts screen or by `:ctx`
    /// must be as read-only as the one the command line started.
    pub(crate) readonly: bool,
}

impl Overrides {
    /// `readonly` is separate because `--readonly` is not a connection flag: it sits on the
    /// top-level command so that `ctx add --readonly` can go on meaning "store this context
    /// read-only".
    pub(crate) fn from_args(conn: &ConnArgs, readonly: bool) -> Overrides {
        Overrides {
            username: conn.username.clone(),
            port: conn.port,
            insecure: conn.insecure,
            ca_bundle: conn.ca_bundle.clone(),
            plain_http: conn.plain_http,
            readonly,
        }
    }
}

/// Cheap to clone: every field is a handle or a small value, which is what lets `connect`
/// hand a copy to a blocking thread instead of reading the config file on the caller's.
#[derive(Clone)]
pub(crate) struct CliContexts {
    pub(crate) config_path: PathBuf,
    /// Shared with the closures that store a verified password.
    pub(crate) secrets: Arc<Secrets>,
    /// The `--host` target with the scrubbed `NUTSH_PASSWORD`, if the command line gave one.
    pub(crate) env_target: Option<(Profile, Option<String>)>,
    /// Whose session this is: the row shown as current. The command line names it first
    /// (`-c` / `NUTSH_CONTEXT`, or the `--host` target), and every connect that succeeds moves
    /// it, so the marker follows a `:ctx` switch without the TUI having to say so. `None`
    /// means nobody has chosen and `current_context` decides; [`ENV_ROW`] is the `--host`
    /// target, which no context can be called.
    ///
    /// Shared across clones on purpose: [`Contexts::connect`] works on a copy of `self`, and
    /// what it selects has to reach the copy the screen lists from.
    pub(crate) selected: Arc<Mutex<Option<String>>>,
    pub(crate) overrides: Overrides,
    /// Whether a session opened through this seam gets a cache directory: `cache = true` in
    /// the config file and no `--no-cache`. Off unless [`CliContexts::caching`] turns it on,
    /// so the `ctx` commands - which open a session only to prove a password - read and write
    /// nothing.
    caching: bool,
    /// Whole-directory expiry for the restore, from the config file.
    cache_max_age: u64,
}

/// A connection ready to run, plus what to do with the password once it is proven.
struct Prepared {
    profile: Profile,
    password: String,
    scope: Scope,
    /// The context name a failure may be blamed on. Not `scope.context`: `Add` connects on
    /// behalf of a context that does not exist yet, so its advice must not tell the user to
    /// re-add a context by that name.
    advice: Option<String>,
    after: Option<After>,
}

/// What runs once the password is proven: it writes, and returns what the user should know.
/// Its errors are warnings, never failures - the session is already up by then.
type After = Box<dyn FnOnce(String) -> anyhow::Result<Vec<String>> + Send>;

impl CliContexts {
    pub(crate) fn new(
        config_path: PathBuf,
        secrets: Secrets,
        env_target: Option<(Profile, Option<String>)>,
        selected: Option<String>,
        overrides: Overrides,
    ) -> CliContexts {
        // A `--host` target is what the command line chose, whatever else it also said.
        let selected = match &env_target {
            Some(_) => Some(ENV_ROW.to_string()),
            None => selected,
        };
        CliContexts {
            config_path,
            secrets: Arc::new(secrets),
            env_target,
            selected: Arc::new(Mutex::new(selected)),
            overrides,
            caching: false,
            cache_max_age: nutsh_core::cache::MAX_AGE,
        }
    }

    /// Give the sessions this seam opens a cache. The TUI turns it on for a run whose config
    /// file and command line both allow one; every other caller leaves it off.
    pub(crate) fn caching(mut self, on: bool, max_age: u64) -> CliContexts {
        self.caching = on;
        self.cache_max_age = max_age;
        self
    }

    /// The current selection, unpoisoned: a panic elsewhere must not cost the user the screen.
    fn selected(&self) -> Option<String> {
        self.selected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn load(&self) -> anyhow::Result<Config> {
        Ok(nutsh_config::load(&self.config_path)?)
    }

    /// The context whose session this is: what was selected this run, else the file's own
    /// pointer. `None` for the `(env)` row, whose session has no context.
    fn current_context(selected: Option<String>, cfg: &Config) -> Option<String> {
        match selected {
            // The `(env)` row is the one marked; no context is.
            Some(name) if name == ENV_ROW => None,
            Some(name) => Some(name),
            // Nothing has been chosen this run, so the file's own pointer answers.
            None => cfg.current_context.clone(),
        }
    }

    /// A context as this command line reaches it. `--plain-http` is checked here rather than
    /// at the connection, so that pointing it at a saved production context is refused while
    /// the password is still inside this process.
    fn profile(&self, ctx: &Context) -> anyhow::Result<Profile> {
        let over = &self.overrides;
        let profile = Profile {
            host: ctx.host.clone(),
            port: over.port.unwrap_or(ctx.port),
            username: over
                .username
                .clone()
                .unwrap_or_else(|| ctx.username.clone()),
            verify_tls: !over.insecure && ctx.verify_tls,
            ca_bundle: over.ca_bundle.clone().or_else(|| ctx.ca_bundle.clone()),
            plain_http: over.plain_http,
        };
        crate::session::guard_plain_http(&profile)?;
        Ok(profile)
    }

    /// The account a password is stored under: the *effective* one, so `-u other` looks up
    /// other's own password rather than sending the context user's under another name.
    fn key(profile: &Profile) -> String {
        nutsh_config::secret_key(&profile.username, &profile.host, profile.port)
    }

    /// `--readonly` only ever adds: a context stored read-only stays read-only whatever the
    /// command line says, and the flag makes every other one read-only for this run.
    fn scope(&self, name: &str, ctx: &Context) -> Scope {
        Scope {
            context: Some(name.to_string()),
            cluster: ctx.cluster.clone(),
            readonly: ctx.readonly || self.overrides.readonly,
        }
    }

    /// Connect with advice attached, the way the CLI commands report the same failures.
    async fn connect_with(
        profile: Profile,
        password: &str,
        scope: Scope,
        advice: Option<&str>,
        restored: Option<&nutsh_core::cache::Restored>,
    ) -> anyhow::Result<Session> {
        nutsh_core::session::connect(&profile, password, scope, restored)
            .await
            .map_err(|e| crate::session::advise(e, advice))
    }

    fn stored(&self, name: String) -> anyhow::Result<Prepared> {
        let cfg = self.load()?;
        let ctx = cfg.get(&name)?.clone();
        let profile = self.profile(&ctx)?;
        let password = match self.secrets.get(&Self::key(&profile))? {
            Some((_, p)) => p,
            None => anyhow::bail!("{}", no_stored_password(&name)),
        };
        Ok(Prepared {
            profile,
            password,
            scope: self.scope(&name, &ctx),
            advice: Some(name),
            after: None,
        })
    }

    /// The session-opening half of "log in": the password is proven by the connection that is
    /// about to be used, and stored once it is up. `nutsh ctx login` proves the same password
    /// the cheaper way, for a run that opens no session - see [`crate::ctx`] for why the two
    /// read a 403 differently.
    fn login(&self, name: String, password: String) -> anyhow::Result<Prepared> {
        let cfg = self.load()?;
        let ctx = cfg.get(&name)?.clone();
        let profile = self.profile(&ctx)?;
        let key = Self::key(&profile);
        let secrets = self.secrets.clone();
        Ok(Prepared {
            profile,
            password,
            scope: self.scope(&name, &ctx),
            advice: Some(name),
            after: Some(Box::new(move |p| {
                store(&secrets, &key, &p)?;
                Ok(Vec::new())
            })),
        })
    }

    /// Verify first, then write: the config file and the secret store are only touched once
    /// the password and the certificate have been accepted by the PC.
    fn add(&self, new: NewContext) -> anyhow::Result<Prepared> {
        anyhow::ensure!(valid_name(&new.name), "invalid context name {:?}", new.name);
        let cfg = self.load()?;
        anyhow::ensure!(
            !cfg.contexts.contains_key(&new.name),
            "context {} already exists",
            new.name
        );
        let ctx = Context {
            host: new.host,
            port: new.port,
            username: new.username,
            verify_tls: !new.insecure,
            ca_bundle: new.ca_bundle,
            cluster: new.cluster,
            readonly: new.readonly,
            skin: None,
        };
        let profile = self.profile(&ctx)?;
        let key = Self::key(&profile);
        let scope = self.scope(&new.name, &ctx);
        let name = new.name;
        let path = self.config_path.clone();
        let secrets = self.secrets.clone();
        Ok(Prepared {
            profile,
            password: new.password,
            scope,
            advice: None,
            after: Some(Box::new(move |p| {
                let mut warnings = Vec::new();
                // Re-read: the file may have changed while the connection was verified.
                let mut cfg = nutsh_config::load(&path)
                    .map_err(|e| anyhow::anyhow!("context {name} was not saved: {e}"))?;
                if cfg.contexts.contains_key(&name) {
                    // Somebody got there first. Their entry is left alone; the password below
                    // is keyed by account, not by context name, so storing it is still right.
                    warnings.push(format!(
                        "context {name} was added meanwhile; not overwritten"
                    ));
                } else {
                    let first = cfg.contexts.is_empty();
                    cfg.contexts.insert(name.clone(), ctx);
                    if first {
                        cfg.current_context = Some(name.clone());
                    }
                    nutsh_config::save(&path, &cfg)
                        .map_err(|e| anyhow::anyhow!("context {name} was not saved: {e}"))?;
                }
                store(&secrets, &key, &p)?;
                Ok(warnings)
            })),
        })
    }

    fn env(&self, password: Option<String>) -> anyhow::Result<Prepared> {
        let (profile, from_env) = self.env_target.clone().ok_or_else(|| {
            anyhow::anyhow!("no --host target: this session was started from a context")
        })?;
        crate::session::guard_plain_http(&profile)?;
        let scope = Scope {
            readonly: self.overrides.readonly,
            ..Scope::default()
        };
        let password = password.or(from_env).ok_or_else(|| {
            anyhow::anyhow!(
                "no password for {}@{}: set NUTSH_PASSWORD or type it here",
                profile.username,
                profile.host
            )
        })?;
        Ok(Prepared {
            profile,
            password,
            scope,
            advice: None,
            after: None,
        })
    }

    /// Everything that touches the config file or the secret store, in one place, so that
    /// `connect` can run all of it on a blocking thread.
    fn prepare(&self, req: ConnectRequest) -> anyhow::Result<Prepared> {
        match req {
            ConnectRequest::Stored { name } => self.stored(name),
            ConnectRequest::Login { name, password } => self.login(name, password),
            ConnectRequest::Add(new) => self.add(new),
            ConnectRequest::Env { password } => self.env(password),
        }
    }
}

/// One phrasing for a password the store would not take, wherever it happens.
fn store(secrets: &Secrets, key: &str, password: &str) -> anyhow::Result<()> {
    secrets
        .set(key, password)
        .map_err(|e| anyhow::anyhow!("the password could not be stored: {e}"))?;
    Ok(())
}

impl Contexts for CliContexts {
    fn list(&self) -> anyhow::Result<Vec<ContextRow>> {
        let selected = self.selected();
        let mut rows = Vec::new();
        if let Some((profile, password)) = &self.env_target {
            rows.push(ContextRow {
                name: ENV_ROW.to_string(),
                host: profile.host.clone(),
                port: profile.port,
                username: profile.username.clone(),
                cluster: None,
                readonly: self.overrides.readonly,
                insecure: !profile.verify_tls,
                current: selected.as_deref() == Some(ENV_ROW),
                has_password: password.is_some(),
            });
        }
        let cfg = self.load()?;
        let current = Self::current_context(selected, &cfg);
        for (name, ctx) in &cfg.contexts {
            // The password column answers "will `Stored` work here", so it asks about the
            // effective account - the one `stored()` would look up. A row this command line
            // cannot reach at all falls back to the context's own key rather than vanishing.
            let key = self
                .profile(ctx)
                .map_or_else(|_| ctx.secret_key(), |p| Self::key(&p));
            rows.push(ContextRow {
                name: name.clone(),
                host: ctx.host.clone(),
                port: self.overrides.port.unwrap_or(ctx.port),
                username: self
                    .overrides
                    .username
                    .clone()
                    .unwrap_or_else(|| ctx.username.clone()),
                cluster: ctx.cluster.clone(),
                readonly: ctx.readonly || self.overrides.readonly,
                insecure: self.overrides.insecure || !ctx.verify_tls,
                current: current.as_deref() == Some(name),
                has_password: self.secrets.locate(&key).is_some(),
            });
        }
        Ok(rows)
    }

    fn connect(&self, req: ConnectRequest) -> BoxFuture<'static, anyhow::Result<Connected>> {
        let this = self.clone();
        Box::pin(async move {
            // Reading the config file and the secret store blocks, and a keychain may even
            // put a dialog in front of the user, so none of it runs on the caller's thread.
            let preparing = this.clone();
            let prepared = tokio::task::spawn_blocking(move || preparing.prepare(req))
                .await
                .map_err(|e| anyhow::anyhow!("prepare task failed: {e}"))??;
            let Prepared {
                profile,
                password,
                scope,
                advice,
                after,
            } = prepared;
            // Before the connection, so it adopts this context's pins instead of negotiating,
            // and so the store the switch builds has rows in it before anything answers. A
            // name that cannot be a directory runs cacheless: there is no terminal to warn on
            // from here, only a frame.
            let (cache_dir, restored) = if this.caching {
                crate::cache::open_for(scope.context.as_deref(), &profile, this.cache_max_age)
                    .map_or((None, None), |(dir, restored)| (Some(dir), restored))
            } else {
                (None, None)
            };
            let session = CliContexts::connect_with(
                profile,
                &password,
                scope,
                advice.as_deref(),
                restored.as_ref(),
            )
            .await?;
            // This is the session now, so this is the row the screen marks. Only a connection
            // that came up moves the marker: a failed one leaves the user where they were.
            this.select(session.context.as_deref());
            // The connection is up. Anything that goes wrong storing what proved it is news
            // for the user, not a reason to throw the session away.
            let warnings = match after {
                None => Vec::new(),
                Some(after) => tokio::task::spawn_blocking(move || after(password))
                    .await
                    .map_err(|e| anyhow::anyhow!("store task failed: {e}"))?
                    .unwrap_or_else(|e| vec![format!("connected, but {e:#}")]),
            };
            Ok(Connected {
                session,
                warnings,
                cache_dir,
                restored,
            })
        })
    }

    fn select(&self, name: Option<&str>) {
        *self.selected.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(name.unwrap_or(ENV_ROW).to_string());
    }

    fn remove(&self, name: &str) -> anyhow::Result<Vec<String>> {
        anyhow::ensure!(
            name != ENV_ROW,
            "the environment target is not saved and cannot be removed"
        );
        let mut cfg = self.load()?;
        // The password to drop is the one the saved context declares: an override changes the
        // account this run uses, not the one the removed context owned.
        let ctx = cfg.get(name)?.clone();
        cfg.contexts.remove(name);
        if cfg.current_context.as_deref() == Some(name) {
            cfg.current_context = None;
        }
        // Save first: a failed save must not leave a context whose password is gone.
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(match self.secrets.delete(&ctx.secret_key()) {
            Ok(warnings) => warnings,
            Err(e) => vec![format!("could not delete the stored password: {e}")],
        })
    }

    fn set_skin(&self, name: &str) -> anyhow::Result<()> {
        let mut cfg = self.load()?;
        // A context that pins its own skin would put it back over `[skin].name` at the next
        // start, so the choice goes where it will be read from.
        let current = Self::current_context(self.selected(), &cfg);
        match current
            .and_then(|c| cfg.contexts.get_mut(&c))
            .filter(|ctx| ctx.skin.is_some())
        {
            Some(ctx) => ctx.skin = Some(name.to_string()),
            None => cfg.skin.name = Some(name.to_string()),
        }
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(())
    }

    fn set_mouse(&self, enabled: bool) -> anyhow::Result<()> {
        let mut cfg = self.load()?;
        cfg.mouse = enabled;
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(())
    }

    fn set_header(&self, header: nutsh_config::Header) -> anyhow::Result<()> {
        let mut cfg = self.load()?;
        cfg.header = header;
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(())
    }

    fn set_log(&self, level: nutsh_config::LogLevel) -> anyhow::Result<()> {
        let mut cfg = self.load()?;
        cfg.log = level;
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(())
    }

    /// A config file that cannot be read gives an empty list rather than an error: the menu is
    /// not the place to report a broken config file, and the Contexts screen already does.
    fn nav_hidden(&self) -> Vec<String> {
        self.load().map(|cfg| cfg.nav.hide).unwrap_or_default()
    }

    fn nav_hide_unserved(&self) -> bool {
        self.load()
            .map(|cfg| cfg.nav.hide_unserved)
            .unwrap_or(false)
    }

    fn settings(&self) -> Vec<Setting> {
        let Ok(cfg) = self.load() else {
            return Vec::new();
        };
        let flags = Flags {
            readonly: self.overrides.readonly,
            no_cache: !self.caching,
            context: Self::current_context(self.selected(), &cfg),
        };
        nutsh_core::contexts::settings_of(&cfg, &flags)
    }

    /// One writer, through `nutsh_config::save`: the atomic 0600 whole-file rewrite `set_skin`
    /// and `set_nav_hidden` already go through, so the file's other sections never move.
    fn set_flag(&self, id: SettingId, on: bool) -> anyhow::Result<()> {
        let mut cfg = self.load()?;
        match id {
            SettingId::Cache => cfg.cache = on,
            SettingId::HideUnserved => cfg.nav.hide_unserved = on,
            // `Skin`, `Mouse` and `Header` keep the writers their own commands use; `Readonly`
            // and `Guardrails` are fixed and the screen never asks.
            other => anyhow::bail!(
                "{} cannot be changed from the settings screen",
                other.label()
            ),
        }
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(())
    }

    fn refresh(&self) -> nutsh_config::Refresh {
        self.load().map(|cfg| cfg.refresh).unwrap_or_default()
    }

    fn set_interval(
        &self,
        at: nutsh_core::contexts::Schedule<'_>,
        every: nutsh_config::Interval,
    ) -> anyhow::Result<()> {
        use nutsh_core::contexts::Schedule;
        let mut cfg = self.load()?;
        match at {
            Schedule::Kind(id) => {
                cfg.refresh.kinds.insert(id.to_string(), every);
            }
            Schedule::Namespace(ns) => {
                cfg.refresh.namespaces.insert(ns.to_string(), every);
            }
            Schedule::Everything => cfg.refresh.default = Some(every),
        }
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(())
    }

    fn set_nav_hidden(&self, hide: &[String]) -> anyhow::Result<()> {
        let mut cfg = self.load()?;
        cfg.nav.hide = hide.to_vec();
        nutsh_config::save(&self.config_path, &cfg)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nutsh_config::{MemoryStore, secret_key};
    use nutsh_mockpc::MockPc;

    fn memory() -> Secrets {
        Secrets::with(vec![Box::new(MemoryStore::default())])
    }

    /// The mock speaks plain HTTP, so every test connects with `--plain-http`.
    fn plain_http() -> Overrides {
        Overrides {
            plain_http: true,
            ..Overrides::default()
        }
    }

    /// `lab` points at the mock; `other` is a saved context on a host nothing answers, which
    /// is what the rows that must not be connected to are made of.
    fn config(pc: &MockPc, dir: &std::path::Path) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            format!(
                "current_context = \"lab\"\n[contexts.lab]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n[contexts.other]\nhost = \"pc.other\"\nusername = \"bob\"\nreadonly = true\n",
                pc.host(),
                pc.port()
            ),
        )
        .unwrap();
        path
    }

    fn env(pc: &MockPc, dir: &std::path::Path) -> CliContexts {
        CliContexts::new(config(pc, dir), memory(), None, None, plain_http())
    }

    fn env_profile(pc: &MockPc) -> Profile {
        Profile {
            host: pc.host(),
            port: pc.port(),
            username: "admin".into(),
            verify_tls: true,
            ca_bundle: None,
            plain_http: true,
        }
    }

    #[test]
    fn lists_config_rows_with_current_and_password_flags() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let cx = env(&pc, dir.path());
        let rows = cx.list().unwrap();
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["lab", "other"]
        );
        assert!(rows[0].current && !rows[1].current);
        assert!(!rows[0].has_password);
        assert!(rows[1].readonly);
        cx.secrets
            .set(&secret_key("admin", &pc.host(), pc.port()), "secret")
            .unwrap();
        assert!(cx.list().unwrap()[0].has_password);
    }

    /// A file that cannot be read is not "no contexts": it is a thing to fix, and saying so
    /// is the only way the user finds out. A file that is not there yet *is* no contexts.
    #[test]
    fn a_broken_config_is_an_error_and_a_missing_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let missing = CliContexts::new(
            dir.path().join("nothing.toml"),
            memory(),
            None,
            None,
            plain_http(),
        );
        assert!(missing.list().unwrap().is_empty());

        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not = [toml").unwrap();
        let broken = CliContexts::new(path, memory(), None, None, plain_http());
        let err = broken.list().unwrap_err();
        assert!(err.to_string().contains("config.toml"), "{err:#}");
    }

    /// `-c lab` on the command line is the row the screen opens on, whatever
    /// `current_context` says; a name that is not there marks nothing rather than guessing.
    #[test]
    fn the_selected_context_is_the_current_row() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let cx = CliContexts::new(
            path.clone(),
            memory(),
            None,
            Some("other".into()),
            plain_http(),
        );
        let rows = cx.list().unwrap();
        assert!(!rows[0].current && rows[1].current);

        let cx = CliContexts::new(path, memory(), None, Some("gone".into()), plain_http());
        assert!(cx.list().unwrap().iter().all(|r| !r.current));
    }

    /// The `*` marker says which session the user is in, so it has to follow the session: a
    /// connect that succeeds moves it - that is what the Contexts screen and `:ctx` do - and a
    /// connect that fails leaves the user where they were.
    #[test]
    fn a_connect_that_succeeds_moves_the_current_marker() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let entry = |name: &str| {
            format!(
                "[contexts.{name}]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\n",
                pc.host(),
                pc.port()
            )
        };
        std::fs::write(
            &path,
            format!(
                "current_context = \"alpha\"\n{}{}",
                entry("alpha"),
                entry("beta")
            ),
        )
        .unwrap();
        let current = |cx: &CliContexts| {
            cx.list()
                .unwrap()
                .into_iter()
                .filter(|r| r.current)
                .map(|r| r.name)
                .collect::<Vec<_>>()
        };

        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());
        assert_eq!(current(&cx), ["alpha"], "the file's own pointer, to start");
        rt.block_on(cx.connect(ConnectRequest::Login {
            name: "beta".into(),
            password: "wrong".into(),
        }))
        .unwrap_err();
        assert_eq!(current(&cx), ["alpha"], "a rejected password moves nothing");

        rt.block_on(cx.connect(ConnectRequest::Login {
            name: "beta".into(),
            password: "secret".into(),
        }))
        .unwrap();
        assert_eq!(current(&cx), ["beta"]);
        // Both accounts are the same `admin@host:port`, so the password beta stored answers
        // for alpha too: switching back is a `Stored` connect, and it moves the marker back.
        rt.block_on(cx.connect(ConnectRequest::Stored {
            name: "alpha".into(),
        }))
        .unwrap();
        assert_eq!(current(&cx), ["alpha"]);

        // The `(env)` row is a row like the others: it starts current because the command line
        // chose it, and it is handed the marker back when a session is opened from it.
        let cx = CliContexts::new(
            path,
            memory(),
            Some((env_profile(&pc), Some("secret".into()))),
            None,
            plain_http(),
        );
        assert_eq!(current(&cx), [ENV_ROW]);
        rt.block_on(cx.connect(ConnectRequest::Login {
            name: "beta".into(),
            password: "secret".into(),
        }))
        .unwrap();
        assert_eq!(current(&cx), ["beta"]);
        rt.block_on(cx.connect(ConnectRequest::Env { password: None }))
            .unwrap();
        assert_eq!(current(&cx), [ENV_ROW]);
    }

    #[test]
    fn env_row_appears_first_when_a_host_target_exists() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        // Through `new`, the way the binary builds it: a `--host` target is what the command
        // line chose, so it is the row that starts current.
        let cx = CliContexts::new(
            config(&pc, dir.path()),
            memory(),
            Some((env_profile(&pc), Some("secret".into()))),
            None,
            plain_http(),
        );
        let rows = cx.list().unwrap();
        assert_eq!(rows[0].name, ENV_ROW);
        assert!(rows[0].has_password && rows[0].current);
        assert!(
            !rows[1].current,
            "the env target is what the command line chose"
        );
        assert!(cx.remove(ENV_ROW).is_err());
        let connected = rt
            .block_on(cx.connect(ConnectRequest::Env { password: None }))
            .unwrap();
        assert_eq!(connected.session.context, None);
        assert!(connected.warnings.is_empty());
    }

    /// The two ways `Env` has nothing to work with, each saying which one it is.
    #[test]
    fn env_without_a_password_or_without_a_target_says_which() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let mut cx = env(&pc, dir.path());
        let err = rt
            .block_on(cx.connect(ConnectRequest::Env { password: None }))
            .unwrap_err();
        assert!(err.to_string().contains("--host"), "{err:#}");

        cx.env_target = Some((env_profile(&pc), None));
        let err = rt
            .block_on(cx.connect(ConnectRequest::Env { password: None }))
            .unwrap_err();
        assert!(err.to_string().contains("NUTSH_PASSWORD"), "{err:#}");
    }

    #[test]
    fn stored_login_add_and_remove_round_trip() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let cx = env(&pc, dir.path());

        let err = rt
            .block_on(cx.connect(ConnectRequest::Stored { name: "lab".into() }))
            .unwrap_err();
        assert!(
            err.to_string().contains("no stored password for lab"),
            "{err:#}"
        );

        let err = rt
            .block_on(cx.connect(ConnectRequest::Login {
                name: "lab".into(),
                password: "wrong".into(),
            }))
            .unwrap_err();
        assert!(err.to_string().contains("rejected"), "{err:#}");
        assert!(
            !cx.list().unwrap()[0].has_password,
            "nothing stored after a rejected login"
        );

        let connected = rt
            .block_on(cx.connect(ConnectRequest::Login {
                name: "lab".into(),
                password: "secret".into(),
            }))
            .unwrap();
        assert_eq!(connected.session.context.as_deref(), Some("lab"));
        assert!(connected.warnings.is_empty());
        assert!(cx.list().unwrap()[0].has_password);
        let connected = rt
            .block_on(cx.connect(ConnectRequest::Stored { name: "lab".into() }))
            .unwrap();
        assert_eq!(connected.session.pc_version.as_deref(), Some("pc.2024.3"));

        let new = NewContext {
            name: "fresh".into(),
            host: pc.host(),
            port: pc.port(),
            username: "admin".into(),
            password: "secret".into(),
            cluster: Some("lab-cluster".into()),
            ca_bundle: None,
            insecure: false,
            readonly: true,
        };
        let connected = rt
            .block_on(cx.connect(ConnectRequest::Add(new.clone())))
            .unwrap();
        assert_eq!(
            connected.session.cluster.as_ref().map(|c| c.name.as_str()),
            Some("lab-cluster")
        );
        assert!(connected.session.readonly);
        assert!(connected.warnings.is_empty());
        assert!(cx.list().unwrap().iter().any(|r| r.name == "fresh"
            && r.has_password
            && r.cluster.as_deref() == Some("lab-cluster")));

        let dup = rt
            .block_on(cx.connect(ConnectRequest::Add(new.clone())))
            .unwrap_err();
        assert!(dup.to_string().contains("exists"), "{dup:#}");
        let bad = rt
            .block_on(cx.connect(ConnectRequest::Add(NewContext {
                name: "fresh2".into(),
                password: "wrong".into(),
                ..new
            })))
            .unwrap_err();
        // No context named `fresh2` exists yet, so the advice must not tell the user to log
        // in to one: a rejected password while adding is just a rejected password.
        assert!(bad.to_string().contains("authentication failed"), "{bad:#}");
        assert!(!bad.to_string().contains("fresh2"), "{bad:#}");
        assert!(
            !cx.list().unwrap().iter().any(|r| r.name == "fresh2"),
            "a rejected password saves nothing"
        );

        assert!(cx.remove("fresh").unwrap().is_empty());
        assert!(!cx.list().unwrap().iter().any(|r| r.name == "fresh"));
        assert!(cx.remove("fresh").is_err());
    }

    /// A name the config file could not hold is refused before anything is written and
    /// before the password goes anywhere near the network.
    #[test]
    fn add_refuses_a_name_the_config_cannot_hold() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let cx = env(&pc, dir.path());
        let err = rt
            .block_on(cx.connect(ConnectRequest::Add(NewContext {
                name: "not a name".into(),
                host: pc.host(),
                port: pc.port(),
                username: "admin".into(),
                password: "secret".into(),
                cluster: None,
                ca_bundle: None,
                insecure: false,
                readonly: false,
            })))
            .unwrap_err();
        assert!(err.to_string().contains("invalid context name"), "{err:#}");
        assert_eq!(cx.list().unwrap().len(), 2, "nothing was added");
    }

    /// `:skin` writes the name back, and nothing else in the file moves.
    #[test]
    fn set_skin_writes_the_name_and_keeps_the_contexts() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());
        cx.set_skin("gruvbox-dark").unwrap();
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.skin.name.as_deref(), Some("gruvbox-dark"));
        assert!(cfg.contexts.contains_key("lab"));
        assert!(cfg.contexts.contains_key("other"));
        assert_eq!(cfg.current_context.as_deref(), Some("lab"));
    }

    /// A context that pins its own skin is what the next start reads, so `:skin` writes there
    /// rather than into a `[skin].name` the pin would cover again.
    #[test]
    fn set_skin_writes_the_current_contexts_skin_when_it_pins_one() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let mut cfg = nutsh_config::load(&path).unwrap();
        cfg.contexts.get_mut("lab").unwrap().skin = Some("nord".into());
        cfg.skin.name = Some("dracula".into());
        nutsh_config::save(&path, &cfg).unwrap();

        // `lab` is `current_context` and pins `nord`: the pin moves, the global name does not.
        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());
        cx.set_skin("gruvbox-dark").unwrap();
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.contexts["lab"].skin.as_deref(), Some("gruvbox-dark"));
        assert_eq!(cfg.skin.name.as_deref(), Some("dracula"));

        // Selected onto a context without a pin, and the global name is the one written.
        let cx = CliContexts::new(
            path.clone(),
            memory(),
            None,
            Some("other".into()),
            plain_http(),
        );
        cx.set_skin("monokai").unwrap();
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.skin.name.as_deref(), Some("monokai"));
        assert_eq!(cfg.contexts["lab"].skin.as_deref(), Some("gruvbox-dark"));
        assert_eq!(cfg.contexts["other"].skin, None);
    }

    /// `:header` writes its setting back on the same terms `:mouse` does, and `auto` takes the
    /// key back out rather than writing the default down.
    #[test]
    fn set_header_writes_the_setting_above_the_contexts_and_auto_removes_it() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());

        cx.set_header(nutsh_config::Header::Compact).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("header = \"compact\""), "{text}");
        assert!(
            text.find("header").unwrap() < text.find("[contexts.lab]").unwrap(),
            "a bare key beneath a table reads as that table's: {text}"
        );
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.header, nutsh_config::Header::Compact);
        assert!(cfg.contexts.contains_key("lab"));
        assert!(cfg.contexts.contains_key("other"));
        assert_eq!(cfg.current_context.as_deref(), Some("lab"));

        cx.set_header(nutsh_config::Header::Auto).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("header"), "{text}");
        assert!(text.contains("[contexts.other]"), "{text}");
    }

    /// `:mouse` writes the answer back, and nothing else in the file moves. `save` rewrites the
    /// whole file, so the bare key has to land above the `[contexts.*]` tables: beneath them
    /// `toml` would read it as the last context's and `deny_unknown_fields` would refuse the
    /// file the next time it was opened.
    #[test]
    fn set_mouse_writes_the_flag_above_the_contexts_and_keeps_them() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());

        cx.set_mouse(false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("mouse = false"), "{text}");
        assert!(
            text.find("mouse").unwrap() < text.find("[contexts.lab]").unwrap(),
            "a bare key beneath a table reads as that table's: {text}"
        );
        let cfg = nutsh_config::load(&path).unwrap();
        assert!(!cfg.mouse);
        assert!(cfg.contexts.contains_key("lab"));
        assert!(cfg.contexts.contains_key("other"));
        assert_eq!(cfg.current_context.as_deref(), Some("lab"));

        // On again, and the key goes rather than being written `true`: a file that never
        // mentioned the mouse goes back to never mentioning it.
        cx.set_mouse(true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("mouse"), "{text}");
        assert!(nutsh_config::load(&path).unwrap().mouse);
    }

    /// `:log` writes through the same whole-file rewrite, and `off` takes the key back out: a
    /// user who turned logging on to read one thing and off again has a file that does not
    /// mention it, which is what the default promises.
    #[test]
    fn set_log_writes_the_level_above_the_contexts_and_off_removes_it() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());

        cx.set_log(nutsh_config::LogLevel::Debug).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("log = \"debug\""), "{text}");
        assert!(
            text.find("log").unwrap() < text.find("[contexts.lab]").unwrap(),
            "a bare key beneath a table reads as that table's: {text}"
        );
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.log, nutsh_config::LogLevel::Debug);
        assert!(cfg.contexts.contains_key("lab") && cfg.contexts.contains_key("other"));
        assert_eq!(cfg.current_context.as_deref(), Some("lab"));

        cx.set_log(nutsh_config::LogLevel::Off).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("log"), "{text}");
        assert!(nutsh_config::load(&path).unwrap().log.is_off());
    }

    /// `:hide` and `:show` recompute the whole `[nav] hide` list and write it back through this
    /// seam. The claim the TUI's own tests cannot make - their fake has nowhere to write - is
    /// that the list survives a `save` that rewrites the whole file: `[nav]` has to land above
    /// the `[contexts.*]` tables, or `toml` would read it as the last context's and
    /// `deny_unknown_fields` would refuse the file the next time it was opened.
    #[test]
    fn set_nav_hidden_writes_the_list_above_the_contexts_and_keeps_them() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());

        cx.set_nav_hidden(&[
            "Data Protection".to_string(),
            "vmm.esxi.config.Vm".to_string(),
        ])
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.find("[nav]").unwrap() < text.find("[contexts.lab]").unwrap(),
            "a section beneath a table nests inside it: {text}"
        );
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.nav.hide, ["Data Protection", "vmm.esxi.config.Vm"]);
        assert_eq!(cx.nav_hidden(), ["Data Protection", "vmm.esxi.config.Vm"]);
        assert!(cfg.contexts.contains_key("lab"));
        assert!(cfg.contexts.contains_key("other"));
        assert_eq!(cfg.current_context.as_deref(), Some("lab"));
        // The rule beside the list is not touched by the list being written.
        assert!(!cfg.nav.hide_unserved);

        // `:show` of the last entry empties the list, and an empty `[nav]` leaves the file
        // without the section rather than with an empty one.
        cx.set_nav_hidden(&[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("nav"), "{text}");
        assert!(cx.nav_hidden().is_empty());
    }

    /// Removing the context the config points at leaves no dangling `current_context`, which
    /// `nutsh_config::load` would reject on the next read.
    #[test]
    fn removing_the_current_context_clears_the_pointer() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = config(&pc, dir.path());
        let cx = CliContexts::new(path.clone(), memory(), None, None, plain_http());
        assert!(cx.remove("lab").unwrap().is_empty());
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.current_context, None);
        assert_eq!(cx.list().unwrap().len(), 1);
    }

    /// `-u other` makes this run other's session: the password looked up, shown, and sent is
    /// other's own, never the context user's under a different name.
    #[test]
    fn a_username_override_keys_the_password_by_the_effective_account() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().credentials("other", "secret").start());
        let dir = tempfile::tempdir().unwrap();
        let cx = CliContexts::new(
            config(&pc, dir.path()),
            memory(),
            None,
            None,
            Overrides {
                username: Some("other".into()),
                ..plain_http()
            },
        );
        assert!(!cx.list().unwrap()[0].has_password);
        cx.secrets
            .set(&secret_key("other", &pc.host(), pc.port()), "secret")
            .unwrap();
        let rows = cx.list().unwrap();
        assert!(rows[0].has_password, "other's password answers for the row");
        assert_eq!(rows[0].username, "other");
        let connected = rt
            .block_on(cx.connect(ConnectRequest::Stored { name: "lab".into() }))
            .unwrap();
        assert_eq!(connected.session.username, "other");
    }

    /// `--plain-http` is a test flag. Pointed at a saved context on a real host it has to be
    /// refused while the password is still in this process, not once it is on the wire.
    #[test]
    fn plain_http_is_refused_for_a_context_that_is_not_loopback() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let cx = env(&pc, dir.path());
        let err = rt
            .block_on(cx.connect(ConnectRequest::Login {
                name: "other".into(),
                password: "secret".into(),
            }))
            .unwrap_err();
        assert!(err.to_string().contains("loopback"), "{err:#}");
    }

    /// A cluster pin that cannot be settled is the context's fault, so the advice names the
    /// context and how to repin it - the same words `--check` uses.
    #[test]
    fn a_cluster_pin_that_cannot_be_settled_names_the_context() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!(
                "[contexts.lab]\nhost = \"{}\"\nport = {}\nusername = \"admin\"\ncluster = \"nope\"\n",
                pc.host(),
                pc.port()
            ),
        )
        .unwrap();
        let cx = CliContexts::new(path, memory(), None, None, plain_http());
        let err = rt
            .block_on(cx.connect(ConnectRequest::Login {
                name: "lab".into(),
                password: "secret".into(),
            }))
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("pins cluster \"nope\""), "{text}");
        assert!(text.contains("--cluster <name> --force"), "{text}");
    }

    /// No secret store at all: the connection is real and is handed over, with the news that
    /// the password did not stick attached to it.
    #[test]
    fn a_password_that_cannot_be_stored_is_a_warning_not_a_failure() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let cx = CliContexts::new(
            config(&pc, dir.path()),
            Secrets::with(Vec::new()),
            None,
            None,
            plain_http(),
        );
        let connected = rt
            .block_on(cx.connect(ConnectRequest::Login {
                name: "lab".into(),
                password: "secret".into(),
            }))
            .unwrap();
        assert_eq!(connected.session.context.as_deref(), Some("lab"));
        assert_eq!(connected.warnings.len(), 1, "{:?}", connected.warnings);
        assert!(
            connected.warnings[0].contains("password could not be stored"),
            "{}",
            connected.warnings[0]
        );
    }

    /// `--readonly` belongs to the run, so it has to reach every session this seam opens -
    /// the one the Contexts screen starts and the one `:ctx` switches to - and every row it
    /// offers, not only the session the command line resolved for itself.
    #[test]
    fn readonly_applies_to_every_row_and_every_session_it_opens() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let overrides = Overrides {
            readonly: true,
            ..plain_http()
        };
        let cx = CliContexts::new(
            config(&pc, dir.path()),
            memory(),
            Some((env_profile(&pc), Some("secret".into()))),
            None,
            overrides,
        );
        // Every row, the `(env)` one included, says what a session opened from it would be.
        assert!(cx.list().unwrap().iter().all(|r| r.readonly));

        let connected = rt
            .block_on(cx.connect(ConnectRequest::Login {
                name: "lab".into(),
                password: "secret".into(),
            }))
            .unwrap();
        assert!(connected.session.readonly, "a context session");
        let connected = rt
            .block_on(cx.connect(ConnectRequest::Env { password: None }))
            .unwrap();
        assert!(connected.session.readonly, "the (env) session");
    }

    /// Without the flag a context decides for itself, which is what makes the flag additive
    /// rather than an override.
    #[test]
    fn without_the_flag_only_the_context_decides() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let cx = env(&pc, dir.path());
        let rows = cx.list().unwrap();
        assert!(!rows[0].readonly, "lab is read-write");
        assert!(rows[1].readonly, "other is stored read-only");
    }
}
