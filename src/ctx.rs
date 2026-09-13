//! `nutsh ctx ...`: manage named Prism Central contexts.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::bail;
use clap::Subcommand;
use nutsh_config::{Config, ConfigError, Context, DEFAULT_PORT, Secrets, valid_name};
use nutsh_core::contexts::Contexts;
use nutsh_prism::{Client, ListOptions, PrismError};

use crate::ConnArgs;
use crate::contexts::{CliContexts, Overrides};

#[derive(Subcommand, Debug)]
pub(crate) enum CtxCommand {
    /// Add a context (refuses to overwrite without --force)
    Add {
        name: String,
        /// Deliberately declares no `env`: clap does not propagate a global into a subcommand
        /// that redefines the same id, so a stray `NUTSH_HOST` cannot silently redirect the
        /// context being written to a host the user never typed.
        #[arg(long)]
        host: String,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        username: String,
        /// Pin every view to this cluster (matched by name at connect time)
        #[arg(long)]
        cluster: Option<String>,
        /// Skip TLS certificate verification for this context
        #[arg(long)]
        insecure: bool,
        #[arg(long, value_name = "FILE")]
        ca_bundle: Option<PathBuf>,
        /// Refuse every mutation in this context
        #[arg(long)]
        readonly: bool,
        #[arg(long)]
        force: bool,
    },
    /// Prompt for the password, verify it against the PC, and store it
    ///
    /// `-u` and `-P` override the context's account exactly as they do when connecting, and
    /// the password is stored under the account that was actually verified.
    Login {
        /// Context to log in to (default: -c, then NUTSH_CONTEXT, then current_context)
        name: Option<String>,
    },
    /// Select the current context
    Use { name: String },
    /// List contexts
    List,
    /// Print one context as TOML
    Show {
        /// Context to print (default: -c, then NUTSH_CONTEXT, then current_context)
        name: Option<String>,
    },
    /// Delete a context and its stored password
    Remove { name: String },
}

/// The `current_context` this file holds, when it names no context in it. The config crate
/// tolerates that on purpose, so that the contexts that *are* there stay listable and
/// repairable; naming it is this command's half of the bargain.
fn dangling(cfg: &nutsh_config::Config) -> Option<&str> {
    let name = cfg.current_context.as_deref()?;
    (!cfg.contexts.contains_key(name)).then_some(name)
}

pub(crate) fn run(
    action: CtxCommand,
    conn: &ConnArgs,
    from_env: Option<String>,
) -> anyhow::Result<i32> {
    let path = nutsh_config::config_path();
    let mut cfg = nutsh_config::load(&path)?;
    let secrets = Secrets::default_stores();
    let mut out = std::io::stdout().lock();
    match action {
        CtxCommand::Add {
            name,
            host,
            port,
            username,
            cluster,
            insecure,
            ca_bundle,
            readonly,
            force,
        } => {
            anyhow::ensure!(valid_name(&name), ConfigError::InvalidName(name.clone()));
            if cfg.contexts.contains_key(&name) && !force {
                bail!(ConfigError::Exists(name));
            }
            let first = cfg.contexts.is_empty();
            let previous = cfg.contexts.get(&name).map(Context::secret_key);
            let ctx = Context {
                host,
                port,
                username,
                verify_tls: !insecure,
                ca_bundle,
                cluster,
                readonly,
                skin: None,
            };
            // `--force` onto a different account leaves the old password stored under a key
            // nothing will ever look up again. Drop it, once the new entry is safely on disk.
            let orphan = previous.filter(|old| *old != ctx.secret_key());
            cfg.contexts.insert(name.clone(), ctx);
            if first {
                cfg.current_context = Some(name.clone());
            }
            nutsh_config::save(&path, &cfg)?;
            writeln!(
                out,
                "added context {name}{}",
                if first { " (current)" } else { "" }
            )?;
            if let Some(old) = orphan {
                warn_orphan(secrets.delete(&old));
            }
        }
        CtxCommand::Login { name } => {
            return login(name, conn, &path, from_env, &secrets, &mut out);
        }
        CtxCommand::Use { name } => {
            cfg.get(&name)?;
            cfg.current_context = Some(name.clone());
            nutsh_config::save(&path, &cfg)?;
            writeln!(out, "current context is {name}")?;
        }
        CtxCommand::List => {
            writeln!(
                out,
                "  {:<12} {:<28} {:<12} {:<16} {:<9} {:<10} SECRET",
                "NAME", "HOST", "USERNAME", "CLUSTER", "TLS", "MODE"
            )?;
            for (name, c) in &cfg.contexts {
                let marker = if cfg.current_context.as_deref() == Some(name) {
                    '*'
                } else {
                    ' '
                };
                let host = if c.port == DEFAULT_PORT {
                    c.host.clone()
                } else {
                    format!("{}:{}", c.host, c.port)
                };
                writeln!(
                    out,
                    "{marker} {:<12} {:<28} {:<12} {:<16} {:<9} {:<10} {}",
                    name,
                    host,
                    c.username,
                    c.cluster.as_deref().unwrap_or("-"),
                    if c.verify_tls { "verify" } else { "insecure" },
                    if c.readonly {
                        "read-only"
                    } else {
                        "read-write"
                    },
                    // Existence only: reading each password here would cost one keyring
                    // unlock prompt per row on macOS.
                    secrets.locate(&c.secret_key()).unwrap_or("none"),
                )?;
            }
            // A pointer at nothing marks no row, and a list where nothing is marked otherwise
            // looks like a list where nothing was ever selected. Saying which name it holds
            // is also saying how to fix it, since `ctx use` is what rewrites it.
            if let Some(name) = dangling(&cfg) {
                writeln!(
                    out,
                    "\ncurrent_context {name} names no context; run `nutsh ctx use <name>`"
                )?;
            }
        }
        CtxCommand::Show { name } => {
            let (name, ctx) = cfg.resolve(selected(&name, conn), None)?;
            let mut one = Config::default();
            one.contexts.insert(name.to_string(), ctx.clone());
            write!(out, "{}", toml::to_string_pretty(&one)?)?;
        }
        CtxCommand::Remove { name } => {
            // Deleting a context is one operation with two front doors - this command and the
            // Contexts screen's own delete - so it lives in one place, behind the seam they
            // share. Nothing here overrides a connection, hence the default `Overrides`.
            let contexts = CliContexts::new(path, secrets, None, None, Overrides::default());
            let warnings = contexts.remove(&name)?;
            writeln!(out, "removed context {name}")?;
            warn_orphan(Ok(warnings));
        }
    }
    Ok(0)
}

/// A positional NAME beats `-c` / `NUTSH_CONTEXT`, which beats `current_context`.
fn selected<'a>(name: &'a Option<String>, conn: &'a ConnArgs) -> Option<&'a str> {
    name.as_deref().or(conn.context.as_deref())
}

/// A password that could not be deleted is a warning, never a failure: the config change it
/// follows is already on disk and already reported, so exiting non-zero here would say the
/// whole command failed when only the cleanup did.
fn warn_orphan(deleted: Result<Vec<String>, ConfigError>) {
    match deleted {
        Ok(warnings) => {
            for w in warnings {
                eprintln!("warning: {w}; a stored password may remain there");
            }
        }
        Err(e) => eprintln!(
            "warning: no secret store could be reached ({e}); a stored password may remain there"
        ),
    }
}

/// Verify a password against the PC, then store it. Nothing is written unless the PC accepted
/// it, so a typo can never become a stored credential that later looks like a changed password.
///
/// This is the second of the two ways a password gets proven, and deliberately not the same as
/// the first. [`crate::contexts::CliContexts::login`] - the Contexts screen's login box, and
/// `:ctx` behind it - opens the whole session, because the screen is about to *use* it: version
/// negotiation across every namespace, the cluster pin settled. This command only answers "is
/// this password right", so it asks one namespace and carves out the 403 that a full connect
/// would never reach, and an account that may not list clusters can still log in.
///
/// The target comes from [`crate::session::target`], the same resolution connecting commands
/// use, so the key written here is by construction the key `--check` will read back - including
/// when `-u`/`-P` (or `NUTSH_USERNAME`/`NUTSH_PORT`) move the effective account off the
/// context's own.
fn login(
    name: Option<String>,
    conn: &ConnArgs,
    config_path: &Path,
    from_env: Option<String>,
    secrets: &Secrets,
    out: &mut impl Write,
) -> anyhow::Result<i32> {
    anyhow::ensure!(
        conn.host.is_none(),
        "ctx login takes a context name, not --host: there is no context to store a password against"
    );
    let mut conn = conn.clone();
    conn.context = name.or(conn.context);
    let target = crate::session::target(&conn, config_path)?;
    let profile = &target.profile;
    let name = target
        .context
        .clone()
        .expect("a context target always names its context");
    let key = target
        .secret_key
        .clone()
        .expect("a context target always has a secret key");
    let password = match from_env {
        Some(p) => p,
        None => {
            anyhow::ensure!(
                std::io::stdin().is_terminal(),
                "stdin is not a terminal; set NUTSH_PASSWORD to log in non-interactively"
            );
            crate::session::prompt_password(&profile.username, &profile.host)?
        }
    };
    let kind = nutsh_catalog::kind("clustermgmt.config.Cluster")
        .ok_or_else(|| anyhow::anyhow!("catalog has no cluster kind"))?;
    /// Everything the PC said about the credential itself stays a `PrismError`, so the match
    /// below reads it unchanged. `Unserved` is the one outcome that is not about the
    /// credential at all: there is no version to send the verifying request to.
    enum Verdict {
        Proven,
        Unserved(String),
    }
    let rt = crate::runtime()?;
    let verified = rt.block_on(async {
        let client = Client::connect(profile, &password)?;
        // One namespace, not twenty: enough to learn which clustermgmt version to ask, and a
        // 401 here is the rejection this command exists to report. The list below is what
        // proves the credential, and its 403 is the case the match handles.
        let status = client.negotiate_namespace("clustermgmt").await?;
        if !status.ok {
            return Ok(Verdict::Unserved(status.detail));
        }
        let opts = ListOptions {
            limit: 1,
            ..Default::default()
        };
        client
            .list_page(kind, 0, &opts)
            .await
            .map(|_| Verdict::Proven)
    });
    match verified {
        Ok(Verdict::Proven) => {}
        // No version of clustermgmt answered, so the password was never put to the PC. Saying
        // "rejected" here would be a lie, and storing it would be a guess.
        Ok(Verdict::Unserved(detail)) => bail!(
            "cannot verify the password: clustermgmt on {} is unavailable ({})",
            profile.host,
            detail
        ),
        // 403 is the PC saying "I know who you are, and this account may not list clusters":
        // the credential is proven. Refusing here would lock every read-restricted account out
        // of `ctx login` entirely. `--check` reads a 403 the same way.
        Err(PrismError::Forbidden(_)) => writeln!(
            out,
            "password accepted (cluster list not permitted for this account)"
        )?,
        Err(PrismError::Auth) => bail!(
            "password for {}@{} rejected; nothing stored",
            profile.username,
            profile.host
        ),
        Err(e) => return Err(crate::session::annotate(e, Some(&name))),
    }
    match secrets.set(&key, &password)? {
        "file" => writeln!(
            out,
            "stored password for {name} in file {} (no keyring available)",
            nutsh_config::FileStore::new(nutsh_config::secrets_dir())
                .path_for(&key)
                .display()
        )?,
        backend => writeln!(out, "stored password for {name} in the {backend}")?,
    }
    Ok(0)
}
