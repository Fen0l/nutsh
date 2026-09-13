//! What a command connects to: a `Target` resolved from flags or a context, and the advice
//! the CLI attaches when connecting through [`nutsh_core::session`] fails.

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context as _, bail};
use nutsh_config::Secrets;
use nutsh_prism::{CertificateReason, PrismError, Profile};

use crate::ConnArgs;

pub(crate) struct Target {
    pub(crate) profile: Profile,
    pub(crate) context: Option<String>,
    pub(crate) cluster: Option<String>,
    pub(crate) readonly: bool,
    /// Account key for the secret store; `None` when connecting by flags.
    pub(crate) secret_key: Option<String>,
}

/// Explicit `--host` wins and bypasses contexts; otherwise `--context` (clap also feeds it from
/// `NUTSH_CONTEXT`), then `current_context`. Flags override individual context fields.
pub(crate) fn target(conn: &ConnArgs, config_path: &Path) -> anyhow::Result<Target> {
    if let Some(host) = &conn.host {
        let username = conn
            .username
            .clone()
            .context("--username (or NUTSH_USERNAME) is required with --host")?;
        let profile = Profile {
            host: host.clone(),
            port: conn.port.unwrap_or(nutsh_config::DEFAULT_PORT),
            username,
            verify_tls: !conn.insecure,
            ca_bundle: conn.ca_bundle.clone(),
            plain_http: conn.plain_http,
        };
        guard_plain_http(&profile)?;
        return Ok(Target {
            profile,
            context: None,
            cluster: None,
            readonly: false,
            secret_key: None,
        });
    }
    let cfg = nutsh_config::load(config_path)?;
    let (name, ctx) = cfg.resolve(conn.context.as_deref(), None)?;
    let profile = Profile {
        host: ctx.host.clone(),
        port: conn.port.unwrap_or(ctx.port),
        username: conn
            .username
            .clone()
            .unwrap_or_else(|| ctx.username.clone()),
        verify_tls: !conn.insecure && ctx.verify_tls,
        ca_bundle: conn.ca_bundle.clone().or_else(|| ctx.ca_bundle.clone()),
        plain_http: conn.plain_http,
    };
    guard_plain_http(&profile)?;
    // Keyed off the *effective* account: `-u other` must look up other's own password rather
    // than send the context user's password under a different name.
    let secret_key = nutsh_config::secret_key(&profile.username, &profile.host, profile.port);
    Ok(Target {
        profile,
        context: Some(name.to_string()),
        cluster: ctx.cluster.clone(),
        readonly: ctx.readonly,
        secret_key: Some(secret_key),
    })
}

pub(crate) fn guard_plain_http(profile: &Profile) -> anyhow::Result<()> {
    if profile.plain_http {
        let loopback = profile.host == "localhost"
            || profile
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        anyhow::ensure!(
            loopback,
            "--plain-http is for local test servers only (host {} is not loopback)",
            profile.host
        );
    }
    Ok(())
}

/// What the secret store had to say about a target. A store that cannot be read is not the
/// same answer as a store that has nothing: a dead keyring told apart from a missing password
/// is the difference between "log in" and "fix your keyring", so the reason is carried
/// alongside rather than swallowed.
pub(crate) struct Stored {
    pub(crate) password: Option<String>,
    /// Why the store could not answer, when that is what happened.
    pub(crate) error: Option<String>,
}

/// The password nobody has to be asked for: `NUTSH_PASSWORD` (already scrubbed from the
/// environment), then the stored secret for the effective account.
pub(crate) fn stored_password(
    target: &Target,
    from_env: Option<String>,
    secrets: &Secrets,
) -> Stored {
    let found = |password| Stored {
        password,
        error: None,
    };
    if let Some(p) = from_env {
        return found(Some(p));
    }
    let Some(key) = target.secret_key.as_ref() else {
        return found(None);
    };
    match secrets.get(key) {
        Ok(hit) => found(hit.map(|(_, p)| p)),
        Err(e) => Stored {
            password: None,
            error: Some(format!("{e}")),
        },
    }
}

/// [`stored_password`], then a hidden prompt when there is somebody to ask. A store that is
/// broken is said out loud either way: it is reported as itself when nobody can be asked, and
/// warned about before the prompt when somebody can, so that typing the password again is a
/// choice rather than a mystery.
pub(crate) fn password(
    target: &Target,
    from_env: Option<String>,
    secrets: &Secrets,
) -> anyhow::Result<String> {
    let stored = stored_password(target, from_env, secrets);
    if let Some(p) = stored.password {
        return Ok(p);
    }
    if !std::io::stdin().is_terminal() {
        if let Some(e) = stored.error {
            bail!("{e}");
        }
        match &target.context {
            Some(c) => bail!(
                "no stored password for context {c} and stdin is not a terminal; run `nutsh ctx login {c}` or set NUTSH_PASSWORD"
            ),
            None => bail!("no password: set NUTSH_PASSWORD or run interactively"),
        }
    }
    if let Some(e) = &stored.error {
        eprintln!("warning: {e}");
    }
    prompt_password(&target.profile.username, &target.profile.host)
}

/// The program's one hidden prompt. Callers test for a terminal themselves, so that the
/// "there is nobody to ask" message can say what they were trying to do.
pub(crate) fn prompt_password(username: &str, host: &str) -> anyhow::Result<String> {
    Ok(rpassword::prompt_password(format!(
        "Password for {username}@{host}: "
    ))?)
}

/// Advice for a failed [`nutsh_core::session::connect`], matching what the CLI has always
/// said: a stale stored password names `ctx login`; a cluster pin that cannot be settled names
/// the context that pins it, since that is what has to change; a rejected certificate names
/// the fix.
pub(crate) fn advise(e: nutsh_core::session::ConnectError, context: Option<&str>) -> anyhow::Error {
    use nutsh_core::session::ConnectError;
    match e {
        ConnectError::Negotiate(e) => annotate(e, context),
        ConnectError::Cluster { name, error } => match (&error, context) {
            (PrismError::Unresolved(_) | PrismError::Ambiguous(_), Some(c)) => {
                let hint = format!(
                    "context {c} pins cluster {name:?}; change it with `nutsh ctx add {c} --cluster <name> --force`"
                );
                annotate(error, context).context(hint)
            }
            _ => annotate(error, context),
        },
    }
}

/// Connect through core with the CLI's advice attached. `restored` is what a previous run left
/// on disk, for the commands that may use it: `--check` passes `None` on purpose, because a
/// probe of every namespace is exactly what it exists to do.
pub(crate) async fn connect(
    target: Target,
    password: &str,
    restored: Option<&nutsh_core::cache::Restored>,
) -> anyhow::Result<nutsh_core::session::Session> {
    let scope = nutsh_core::session::Scope {
        context: target.context.clone(),
        cluster: target.cluster.clone(),
        readonly: target.readonly,
    };
    nutsh_core::session::connect(&target.profile, password, scope, restored)
        .await
        .map_err(|e| advise(e, target.context.as_deref()))
}

pub(crate) fn annotate(e: PrismError, context: Option<&str>) -> anyhow::Error {
    match (&e, context) {
        (PrismError::Auth, Some(c)) => {
            anyhow::anyhow!("stored password for {c} rejected; run `nutsh ctx login {c}`")
        }
        (PrismError::Certificate { reason, .. }, _) => {
            let advice = certificate_advice(reason, context);
            anyhow::anyhow!("{e}\n  {advice}")
        }
        _ => e.into(),
    }
}

/// What to do about a rejected certificate. Prism Central ships a self-signed certificate
/// flagged as a CA, which rustls refuses as a server certificate even when it is trusted, so
/// `--ca-bundle` is only offered where it can work.
fn certificate_advice(reason: &CertificateReason, context: Option<&str>) -> String {
    let skip = match context {
        Some(c) => format!(
            "skip verification by re-adding the context with its flags plus --insecure: `nutsh ctx add {c} ... --insecure --force`"
        ),
        None => "skip verification with --insecure".to_string(),
    };
    match reason {
        CertificateReason::CaUsedAsEndEntity => format!(
            "--ca-bundle cannot trust a CA certificate: install a CA-signed certificate on Prism Central, or {skip}"
        ),
        CertificateReason::UnknownIssuer => {
            format!("trust its CA with --ca-bundle <pem>, or {skip}")
        }
        CertificateReason::NotValidForName => {
            format!("connect with a host name the certificate covers, or {skip}")
        }
        CertificateReason::Expired => format!("renew the certificate on Prism Central, or {skip}"),
        CertificateReason::Other(_) => skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(reason: CertificateReason) -> PrismError {
        PrismError::Certificate {
            host: "pc.lab".into(),
            reason,
        }
    }

    #[test]
    fn ca_certificate_advice_rules_out_ca_bundle_and_names_the_context() {
        let text = format!(
            "{:#}",
            annotate(rejected(CertificateReason::CaUsedAsEndEntity), Some("lab"))
        );
        assert!(
            text.starts_with(
                "TLS certificate of pc.lab rejected: the server presented a CA certificate"
            ),
            "{text}"
        );
        assert!(text.contains("--ca-bundle cannot trust"), "{text}");
        assert!(
            text.ends_with("`nutsh ctx add lab ... --insecure --force`"),
            "{text}"
        );
    }

    #[test]
    fn unknown_issuer_advice_offers_ca_bundle_then_insecure() {
        let text = format!(
            "{:#}",
            annotate(rejected(CertificateReason::UnknownIssuer), None)
        );
        assert!(text.contains("--ca-bundle <pem>"), "{text}");
        assert!(
            text.ends_with("skip verification with --insecure"),
            "{text}"
        );
        assert!(!text.contains("ctx add"), "{text}");
    }

    /// The cluster arm blames the pin, not the connection: the context that names the
    /// cluster is the thing the user has to change, so the advice says which and how.
    #[test]
    fn a_cluster_that_cannot_be_settled_names_the_context_that_pins_it() {
        let unresolved = || nutsh_core::session::ConnectError::Cluster {
            name: "nope".into(),
            error: PrismError::Unresolved("cluster \"nope\" not found; known: prod".into()),
        };
        let text = format!("{:#}", advise(unresolved(), Some("lab")));
        assert!(text.contains("context lab pins cluster \"nope\""), "{text}");
        assert!(
            text.contains("`nutsh ctx add lab --cluster <name> --force`"),
            "{text}"
        );
        assert!(text.contains("not found; known: prod"), "{text}");

        // Nothing pins anything when the cluster came from a flag, so there is no repin to
        // advise: the error stands on its own.
        let text = format!("{:#}", advise(unresolved(), None));
        assert!(!text.contains("pins cluster"), "{text}");
    }

    /// A failed negotiation is advised exactly as `annotate` advises it; the step adds nothing.
    #[test]
    fn a_failed_negotiation_keeps_the_advice_annotate_gives_it() {
        let text = format!(
            "{:#}",
            advise(
                nutsh_core::session::ConnectError::Negotiate(PrismError::Auth),
                Some("lab")
            )
        );
        assert_eq!(
            text,
            format!("{:#}", annotate(PrismError::Auth, Some("lab")))
        );
        assert!(text.contains("`nutsh ctx login lab`"), "{text}");
    }

    #[test]
    fn other_errors_pass_through_unchanged() {
        let text = format!(
            "{:#}",
            annotate(PrismError::Forbidden("x".into()), Some("lab"))
        );
        assert_eq!(text, "forbidden (HTTP 403): x");
    }
}
