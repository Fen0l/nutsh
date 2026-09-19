//! A connected Prism Central: the negotiated client, the resolved cluster pin, and what the
//! header shows. Shared by the TUI and `--check`.

use std::sync::Arc;

use nutsh_prism::{Client, ClusterRef, NamespaceStatus, PrismError, Profile};

use crate::cache;

#[derive(Clone)]
pub struct Session {
    pub client: Arc<Client>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub context: Option<String>,
    pub cluster: Option<ClusterRef>,
    pub readonly: bool,
    pub insecure: bool,
    /// From the domain manager, e.g. `pc.2024.3`; `None` when it could not be read.
    pub pc_version: Option<String>,
    /// The domain manager's `extId`, which is what ties a cache directory to a Prism Central.
    pub domain_manager: Option<String>,
    /// What became of the cache this session was handed.
    pub cache: CacheOutcome,
}

/// What became of the cache this session was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    /// None was offered: `cache = false`, `--no-cache`, `--check`, or nothing on disk.
    NotOffered,
    /// The pins and the cluster came from disk, and no negotiation ran.
    Adopted,
    /// The Prism Central identified itself and it is not the one that directory belongs to.
    /// The caller resets the store, removes the directory, and the session is a fresh one.
    Rejected,
}

/// By hand, because `Client` holds the password: only the fields above it are printed.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("context", &self.context)
            .field("cluster", &self.cluster)
            .field("readonly", &self.readonly)
            .field("insecure", &self.insecure)
            .field("pc_version", &self.pc_version)
            .field("domain_manager", &self.domain_manager)
            .finish_non_exhaustive()
    }
}

/// What `connect` was asked for, beyond the profile.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub context: Option<String>,
    pub cluster: Option<String>,
    pub readonly: bool,
}

/// Where a connection failed, so the binary can add advice that depends on the step.
#[derive(Debug)]
pub enum ConnectError {
    Negotiate(PrismError),
    /// Resolving the pinned cluster name failed; the name is carried for the advice.
    Cluster {
        name: String,
        error: PrismError,
    },
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Negotiate(e) => write!(f, "{e}"),
            ConnectError::Cluster { error, .. } => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ConnectError {
    /// Which step failed is this error's whole contribution; the reason is the `PrismError`
    /// under it, so anything that walks the chain still reaches it.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConnectError::Negotiate(e) => Some(e),
            ConnectError::Cluster { error, .. } => Some(error),
        }
    }
}

/// The verdicts that belong to the host rather than to one request: exactly the errors
/// `probe_namespace` aborts a whole negotiation on. They are the ones the restore path has to
/// fail on, so that a rejected password reads as "run `nutsh ctx login lab`" rather than as a
/// session whose every pane is empty.
fn host_wide(e: &PrismError) -> bool {
    matches!(
        e,
        PrismError::Auth
            | PrismError::Certificate { .. }
            | PrismError::Tls(_)
            | PrismError::Connect { .. }
            | PrismError::Transport(_)
            | PrismError::RateLimited { .. }
    )
}

/// Only an identity that **answered and differed** invalidates. A domain manager that did not
/// answer - no such endpoint, not permitted, a body that will not decode - is `None` here, so a
/// flaky one must not cost the user their cache.
fn verdict(answered: Option<&str>, stored: Option<&str>) -> CacheOutcome {
    match (answered, stored) {
        (Some(a), Some(s)) if a != s => CacheOutcome::Rejected,
        _ => CacheOutcome::Adopted,
    }
}

/// Connect, adopt or negotiate API versions, read the PC identity, resolve the pinned cluster.
pub async fn connect(
    profile: &Profile,
    password: &str,
    scope: Scope,
    restored: Option<&cache::Restored>,
) -> Result<Session, ConnectError> {
    let client = Client::connect(profile, password).map_err(ConnectError::Negotiate)?;
    let mut outcome = CacheOutcome::NotOffered;
    match restored {
        Some(r) => {
            // Zero requests, and the largest latency win in the program: a context switch
            // currently pays the full 20-to-180-request negotiation.
            client.adopt_pins(&r.pins, adoptable(r, cache::now_secs()));
            outcome = CacheOutcome::Adopted;
        }
        None => {
            client.negotiate().await.map_err(ConnectError::Negotiate)?;
        }
    }
    let identity = match restored {
        // The one request the adopted path makes, and therefore the session's only chance to
        // notice a rejected password or a refused certificate: the negotiation that would
        // surface them does not run. Anything narrower than a host-wide verdict - a 404, a 403,
        // a body that will not decode - stays "did not answer" and keeps the cache.
        Some(_) => match client.pc_identity_result().await {
            Ok(identity) => identity,
            Err(e) if host_wide(&e) => return Err(ConnectError::Negotiate(e)),
            Err(_) => None,
        },
        None => client.pc_identity().await,
    };
    let (domain_manager, pc_version) = match identity {
        Some((ext_id, version)) => (Some(ext_id), version),
        None => (None, None),
    };
    if let Some(r) = restored {
        outcome = verdict(
            domain_manager.as_deref(),
            r.identity.domain_manager.as_deref(),
        );
        if outcome == CacheOutcome::Rejected {
            // Same host name, a different Prism Central: everything restored is wrong, and the
            // full negotiation is the only way back.
            client.negotiate().await.map_err(ConnectError::Negotiate)?;
        }
    }
    let restored_cluster = restored
        .filter(|_| outcome == CacheOutcome::Adopted)
        .and_then(|r| r.cluster.as_ref());
    let cluster = match (&scope.cluster, restored_cluster) {
        // One request fewer: the resolved cluster is in the file - but only when it is the
        // cluster this session asked for. A cache directory is keyed by context name, and
        // `nutsh ctx add lab --cluster other --force` re-pins the context without changing
        // that key, so the stored name has to match the requested one the way
        // `resolve_cluster` matches it: exactly, then ignoring ASCII case.
        (Some(name), Some(c)) if c.name.eq_ignore_ascii_case(name) => Some(ClusterRef {
            ext_id: c.ext_id.clone(),
            name: c.name.clone(),
        }),
        (Some(name), _) => match client.resolve_cluster(name).await {
            Ok(c) => Some(c),
            Err(error) => {
                return Err(ConnectError::Cluster {
                    name: name.clone(),
                    error,
                });
            }
        },
        (None, _) => None,
    };
    Ok(Session {
        client: Arc::new(client),
        host: profile.host.clone(),
        port: profile.port,
        username: profile.username.clone(),
        context: scope.context,
        cluster,
        readonly: scope.readonly,
        insecure: !profile.verify_tls,
        pc_version,
        domain_manager,
        cache: outcome,
    })
}

/// The restored statuses this run may adopt: **positives only**, and only while they are
/// under a day old. A restored `ok: false` is dropped and that namespace reads as unknown
/// until a live answer says otherwise; see `Client::adopt_pins` for why the rule is
/// asymmetric.
fn adoptable(r: &cache::Restored, now: u64) -> Vec<NamespaceStatus> {
    r.namespaces
        .iter()
        .filter(|n| n.ok && now.saturating_sub(n.at) <= cache::NAMESPACE_TTL)
        .filter_map(|n| {
            let ns = nutsh_catalog::NAMESPACES
                .iter()
                .find(|c| c.name == n.name)?;
            let pinned = n
                .pinned
                .as_ref()
                .and_then(|p| ns.versions.iter().find(|v| **v == p.as_str()).copied());
            Some(NamespaceStatus {
                name: ns.name,
                version: ns.version,
                pinned,
                ok: true,
                // Marked, because it is a claim about the *last* run: nothing this session
                // probed that path, and a row reading `probed /vmm/v4.3/config/vms` would say
                // otherwise to whoever reads it next. The mark is the flag and never the
                // string, or it would be written back into the record it came from and grow a
                // `restored: ` every run.
                //
                // The trim is for the files that already did: a cache written by the version
                // that prefixed the string heals on the first run that reads it.
                detail: n.detail.trim_start_matches("restored: ").to_string(),
                restored: true,
            })
        })
        .collect()
}

/// One namespace status as `meta.json` keeps it. The other half of `adoptable`, and here
/// beside it so that what is written and what is read back stay one decision.
///
/// `restored` is deliberately not persisted. A record says what was learned, not who is
/// reading it: the run that reads this file is the one that knows the answer came from disk,
/// and it sets the flag itself.
pub fn recorded(status: &NamespaceStatus, now: u64) -> cache::NamespaceRecord {
    cache::NamespaceRecord {
        name: status.name.to_string(),
        version: status.version.to_string(),
        pinned: status.pinned.map(str::to_string),
        ok: status.ok,
        detail: status.detail.clone(),
        at: now,
    }
}

impl Session {
    /// `context lab  pc.lab.example pc.2024.3  cluster prod-01 (0006...)  read-write [insecure]`
    pub fn header(&self) -> String {
        let cluster = match &self.cluster {
            Some(c) => format!("{} ({})", c.name, c.ext_id),
            None => "-".to_string(),
        };
        let version = self
            .pc_version
            .as_ref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        let mode = if self.readonly {
            "read-only"
        } else {
            "read-write"
        };
        let insecure = if self.insecure { " [insecure]" } else { "" };
        format!(
            "context {}  {}{version}  cluster {}  {mode}{insecure}",
            self.context.as_deref().unwrap_or("-"),
            self.host,
            cluster,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// `Client::connect` builds a reqwest client without talking to anything, so none of these
    /// need a server.
    fn client(password: &str) -> Client {
        Client::connect(
            &Profile {
                host: "pc.lab".into(),
                port: 9440,
                username: "admin".into(),
                verify_tls: false,
                ca_bundle: None,
                plain_http: false,
            },
            password,
        )
        .unwrap()
    }

    fn record(
        name: &str,
        version: &str,
        pinned: Option<&str>,
        ok: bool,
        at: u64,
    ) -> cache::NamespaceRecord {
        cache::NamespaceRecord {
            name: name.into(),
            version: version.into(),
            pinned: pinned.map(str::to_string),
            ok,
            detail: "probed /vmm/v4.3/ahv/config/vms".into(),
            at,
        }
    }

    fn restored(namespaces: Vec<cache::NamespaceRecord>) -> cache::Restored {
        cache::Restored {
            dir: std::path::PathBuf::from("/nowhere"),
            written: 0,
            identity: cache::Identity {
                host: "pc.lab".into(),
                port: 9440,
                username: "admin".into(),
                domain_manager: None,
                pc_version: None,
            },
            pins: BTreeMap::new(),
            namespaces,
            cluster: None,
            names: Vec::new(),
            stats: None,
            sample: None,
            can_i: None,
            tables: Vec::new(),
        }
    }

    fn session(client: Client) -> Session {
        Session {
            client: Arc::new(client),
            host: "pc.lab".into(),
            port: 9440,
            username: "admin".into(),
            context: Some("lab".into()),
            cluster: Some(ClusterRef {
                ext_id: "0006".into(),
                name: "prod-01".into(),
            }),
            readonly: true,
            insecure: true,
            pc_version: Some("pc.2024.3".into()),
            domain_manager: None,
            cache: CacheOutcome::NotOffered,
        }
    }

    #[test]
    fn header_shows_every_field() {
        let s = session(client("x"));
        assert_eq!(
            s.header(),
            "context lab  pc.lab pc.2024.3  cluster prod-01 (0006)  read-only [insecure]"
        );
        let s = Session {
            context: None,
            cluster: None,
            readonly: false,
            insecure: false,
            pc_version: None,
            ..s
        };
        assert_eq!(s.header(), "context -  pc.lab  cluster -  read-write");
    }

    /// Only an identity that **answered and differed** invalidates. `pc_identity` swallows its
    /// errors, so "did not answer" and "is a different Prism Central" are distinguishable and
    /// must be distinguished: a flaky domain manager must not cost the user their cache.
    #[test]
    fn the_verdict_needs_an_answer_that_differs() {
        assert_eq!(verdict(Some("dm-1"), Some("dm-1")), CacheOutcome::Adopted);
        assert_eq!(
            verdict(None, Some("dm-1")),
            CacheOutcome::Adopted,
            "no answer keeps it"
        );
        assert_eq!(
            verdict(Some("dm-1"), None),
            CacheOutcome::Adopted,
            "nothing stored to differ from"
        );
        assert_eq!(verdict(Some("dm-2"), Some("dm-1")), CacheOutcome::Rejected);
    }

    /// Positives only, still fresh only, and re-resolved against the *current* catalog: the
    /// three rules §5.3 gives the restore path, in the order they apply.
    #[test]
    fn adoptable_takes_the_fresh_positives_and_marks_them_as_restored() {
        let now = 1_000_000;
        let r = restored(vec![
            record("vmm", "v4.3", Some("v4.3"), true, now),
            record("iam", "v4.0", Some("v4.0"), false, now),
            record(
                "clustermgmt",
                "v4.3",
                Some("v4.3"),
                true,
                now - cache::NAMESPACE_TTL - 1,
            ),
        ]);
        let adopted = adoptable(&r, now);
        assert_eq!(
            adopted.iter().map(|s| s.name).collect::<Vec<_>>(),
            ["vmm"],
            "the negative is dropped, and so is the row older than a day"
        );
        assert_eq!(adopted[0].pinned, Some("v4.3"));
        assert!(
            adopted[0].restored,
            "a row must not claim this run probed anything"
        );
        assert_eq!(
            adopted[0].detail, "probed /vmm/v4.3/ahv/config/vms",
            "and the detail is the fact, unadorned"
        );
    }

    /// Ten runs against one context leave `meta.json` saying exactly what one run left.
    ///
    /// A marker kept as a prefix on the detail string would be copied back into the record it
    /// came from: `restored: restored: restored: probed /aiops/v4.0/config/scenarios`, ten
    /// characters longer every run, for as long as the context survived. A flag has nowhere to
    /// accumulate.
    #[test]
    fn a_record_survives_ten_round_trips_without_growing() {
        let now = 1_000_000;
        let first = record("vmm", "v4.3", Some("v4.3"), true, now);
        let mut records = vec![first.clone()];
        for run in 1..=10 {
            let adopted = adoptable(&restored(records.clone()), now);
            assert_eq!(adopted.len(), 1, "run {run}");
            records = adopted.iter().map(|s| recorded(s, now)).collect();
            assert_eq!(records[0], first, "run {run} wrote something else back");
        }
    }

    /// And a file carrying accumulated markers heals on the first run that reads it, rather
    /// than keeping them for ever because they are already there.
    #[test]
    fn an_already_marked_record_is_read_back_clean() {
        let now = 1_000_000;
        let mut stale = record("vmm", "v4.3", Some("v4.3"), true, now);
        stale.detail = format!("{}{}", "restored: ".repeat(8), stale.detail);
        let adopted = adoptable(&restored(vec![stale]), now);
        assert_eq!(adopted[0].detail, "probed /vmm/v4.3/ahv/config/vms");
        assert!(adopted[0].restored);
    }

    /// A pin the namespace no longer offers costs the pin, not the row: the namespace is still
    /// known to be served, and routes at the catalog's version until something says otherwise.
    #[test]
    fn a_version_the_catalog_dropped_comes_back_unpinned() {
        let now = 1_000_000;
        let r = restored(vec![record("vmm", "v4.3", Some("v9.9"), true, now)]);
        let adopted = adoptable(&r, now);
        assert_eq!(adopted.len(), 1);
        assert_eq!(adopted[0].pinned, None);
        assert_eq!(adopted[0].version, "v4.3", "the catalog's, not the file's");
    }

    /// A namespace this build has never heard of cannot be routed at all, so it does not
    /// become a row that claims it can.
    #[test]
    fn a_namespace_the_catalog_dropped_vanishes() {
        let now = 1_000_000;
        let r = restored(vec![record("nosuch", "v4.0", Some("v4.0"), true, now)]);
        assert!(adoptable(&r, now).is_empty());
    }

    /// A directory between a day and a week old adopts pins with no status left to adopt, and
    /// that must not read as "nothing is served": no pane would open, so no request would go
    /// out, so nothing could correct it. Unknown is not unavailable.
    #[test]
    fn a_restore_whose_statuses_have_all_expired_still_routes() {
        let now = 1_000_000;
        let mut r = restored(vec![record(
            "vmm",
            "v4.3",
            Some("v4.3"),
            true,
            now - cache::NAMESPACE_TTL - 1,
        )]);
        r.pins = [("vmm".to_string(), "v4.3".to_string())]
            .into_iter()
            .collect();
        let adopted = adoptable(&r, now);
        assert!(adopted.is_empty(), "every row is past its day");

        let client = client("x");
        client.adopt_pins(&r.pins, adopted);
        let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
        assert!(
            client.is_served(vms),
            "the kind is unknown, which is a reason to ask, not a reason to hide it"
        );
    }

    /// A `ConnectError` says which step failed; the reason has to stay reachable under it.
    #[test]
    fn connect_error_keeps_the_prism_error_as_its_source() {
        let e = ConnectError::Cluster {
            name: "prod".into(),
            error: PrismError::Unresolved("cluster \"prod\" not found".into()),
        };
        let source = std::error::Error::source(&e).expect("a source");
        assert!(source.to_string().contains("not found"), "{source}");
        assert_eq!(
            e.to_string(),
            source.to_string(),
            "the step adds no text of its own"
        );
    }

    /// The password lives in the client; `{:?}` on a session must not be a way to reach it.
    #[test]
    fn debug_hides_the_client() {
        let text = format!("{:?}", session(client("hunter2")));
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("host: \"pc.lab\""), "{text}");
    }
}
