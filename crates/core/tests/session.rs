//! `session::connect` when a cache is offered: what it still checks before adopting one, and
//! which cluster the adopted directory is allowed to speak for.

use std::collections::BTreeMap;
use std::path::PathBuf;

use nutsh_core::cache;
use nutsh_core::session::{self, CacheOutcome, ConnectError, Scope};
use nutsh_mockpc::MockPc;
use nutsh_prism::{PrismError, Profile};

fn profile(pc: &MockPc) -> Profile {
    Profile {
        host: pc.host(),
        port: pc.port(),
        username: "admin".into(),
        verify_tls: true,
        ca_bundle: None,
        plain_http: true,
    }
}

/// What the last run left behind: pins for every namespace it reached, and the cluster it
/// resolved, which was `prod-01`.
fn restored(pc: &MockPc) -> cache::Restored {
    let now = cache::now_secs();
    let namespaces: Vec<cache::NamespaceRecord> = nutsh_catalog::NAMESPACES
        .iter()
        .map(|ns| cache::NamespaceRecord {
            name: ns.name.into(),
            version: ns.version.into(),
            pinned: Some(ns.version.into()),
            ok: true,
            detail: "probed".into(),
            at: now,
        })
        .collect();
    cache::Restored {
        dir: PathBuf::from("/nowhere"),
        written: now,
        identity: cache::Identity {
            host: pc.host(),
            port: pc.port(),
            username: "admin".into(),
            domain_manager: None,
            pc_version: None,
        },
        pins: namespaces
            .iter()
            .map(|n| (n.name.clone(), n.version.clone()))
            .collect::<BTreeMap<String, String>>(),
        namespaces,
        cluster: Some(cache::ClusterRecord {
            ext_id: "stored-ext-id".into(),
            name: "prod-01".into(),
        }),
        names: Vec::new(),
        stats: None,
        sample: None,
        can_i: None,
        tables: Vec::new(),
    }
}

/// The adopted path makes exactly one request - the domain manager read - so it is the only
/// place left where a rejected password can be noticed at all: the negotiation that would
/// surface a 401 does not run. Without this, `nutsh vm` would start a TUI whose every pane
/// fails instead of saying which context to log in to again.
#[tokio::test]
async fn a_rejected_password_fails_the_connect_even_under_an_adopted_cache() {
    let pc = MockPc::builder().start().await;
    let r = restored(&pc);
    let e = session::connect(&profile(&pc), "wrong", Scope::default(), Some(&r))
        .await
        .expect_err("a 401 is not a session");
    assert!(
        matches!(e, ConnectError::Negotiate(PrismError::Auth)),
        "{e:?}"
    );
}

/// And the same restore with the right password: one request for the whole connection.
#[tokio::test]
async fn an_adopted_cache_connects_on_a_single_request() {
    let pc = MockPc::builder().start().await;
    let r = restored(&pc);
    let s = session::connect(&profile(&pc), "secret", Scope::default(), Some(&r))
        .await
        .expect("the restore is adopted");
    assert_eq!(s.cache, CacheOutcome::Adopted);
    assert!(s.domain_manager.is_some(), "the identity was read");
    assert_eq!(
        pc.requests().len(),
        1,
        "the domain manager read, and no negotiation behind it: {:?}",
        pc.requests()
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>()
    );
}

/// A cache directory is keyed by context name, and `nutsh ctx add lab --cluster other --force`
/// re-pins the context without changing that key. The stored cluster therefore has to answer
/// to the name this session asked for, or the header and every cluster-scoped counter would
/// quietly stay on the old cluster for as long as the directory lives.
#[tokio::test]
async fn the_stored_cluster_is_adopted_only_under_the_name_the_session_asked_for() {
    let pc = MockPc::builder().start().await;
    let r = restored(&pc);

    let scope = Scope {
        cluster: Some("PROD-01".into()),
        ..Default::default()
    };
    let s = session::connect(&profile(&pc), "secret", scope, Some(&r))
        .await
        .expect("the restore is adopted");
    assert_eq!(
        s.cluster.as_ref().map(|c| c.ext_id.as_str()),
        Some("stored-ext-id"),
        "the same name, matched the way `resolve_cluster` matches it"
    );
    assert_eq!(pc.requests().len(), 1, "and no cluster list behind it");

    // Another cluster entirely: the file's answer is not this session's, so it is resolved live.
    let scope = Scope {
        cluster: Some("lab-cluster".into()),
        ..Default::default()
    };
    let s = session::connect(&profile(&pc), "secret", scope, Some(&r))
        .await
        .expect("the live resolution answers");
    let cluster = s.cluster.expect("a cluster");
    assert_eq!(cluster.name, "lab-cluster");
    assert_ne!(cluster.ext_id, "stored-ext-id");
}
