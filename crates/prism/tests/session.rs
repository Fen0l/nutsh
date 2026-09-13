//! Adopting what the last run negotiated, and repairing one namespace when it was wrong.

use std::collections::BTreeMap;
use std::sync::Arc;

use nutsh_mockpc::MockPc;
use nutsh_prism::{Availability, Client, ListOptions, NamespaceStatus, PrismError, Profile};

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

fn pins(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
        .collect()
}

/// A restored positive, as the cache hands it back.
fn restored(name: &'static str, pinned: &'static str) -> NamespaceStatus {
    NamespaceStatus {
        name,
        version: "v4.3",
        pinned: Some(pinned),
        ok: true,
        detail: "probed /vmm/v4.3/ahv/config/vms".into(),
        restored: true,
    }
}

fn vms() -> &'static nutsh_catalog::Kind {
    nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs")
}

/// The largest latency win in the program: a context switch currently pays the full
/// 20-to-180-request negotiation, and an adopted pin pays none of it.
#[tokio::test]
async fn adopting_pins_issues_no_request_at_all() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    client.adopt_pins(&pins(&[("vmm", "v4.3")]), vec![restored("vmm", "v4.3")]);
    assert_eq!(client.metrics().started(), 0, "not one request");
    assert!(
        client.is_served(vms()),
        "and the sidebar can already draw it"
    );
    assert_eq!(client.statuses().len(), 1);
    assert!(pc.requests().is_empty());
}

/// The asymmetry of §5.3, which is what keeps a restore from bricking a session: a namespace
/// the restore says nothing about - it was down when the cache was written, or its whole row
/// aged past its day - is *unknown*, not unavailable. It reads as served, so a request is made,
/// so something can correct it. A false "not served" would suppress the pane with no request
/// ever made, and nothing could.
#[tokio::test]
async fn a_namespace_the_restore_is_silent_about_reads_as_unknown_not_as_unavailable() {
    let pc = MockPc::builder().start().await;
    let client = Client::connect(&profile(&pc), "secret").unwrap();
    let clusters = nutsh_catalog::kind("clustermgmt.config.Cluster").expect("clusters");
    // Not negotiated and nothing adopted: there is no reason to believe anything is served.
    assert!(!client.is_served(clusters));

    client.adopt_pins(&pins(&[("vmm", "v4.3")]), vec![restored("vmm", "v4.3")]);
    assert!(
        client.is_served(clusters),
        "a restore that is silent about clustermgmt does not make it unavailable"
    );
    assert_eq!(client.availability(clusters), Availability::Served);
    assert_eq!(client.metrics().started(), 0, "and still not one request");
    // Unknown means "ask": the list goes out at the catalog's version, and the answer settles it.
    assert!(
        client
            .list_page(clusters, 0, &ListOptions::default())
            .await
            .is_ok()
    );
}

/// A pin naming a namespace the catalog no longer declares, or a version that namespace no
/// longer offers, is dropped on its own; the rest stand.
#[tokio::test]
async fn a_pin_the_catalog_cannot_resolve_is_dropped_alone() {
    let pc = MockPc::builder().start().await;
    let client = Client::connect(&profile(&pc), "secret").unwrap();
    client.adopt_pins(
        &pins(&[("vmm", "v4.3"), ("nosuch", "v4.0"), ("iam", "v9.9")]),
        vec![restored("vmm", "v4.3")],
    );
    assert!(
        client
            .list_page(vms(), 0, &ListOptions::default())
            .await
            .is_ok(),
        "the pin that resolves still routes"
    );
    assert!(
        pc.requests_to("/ahv/config/vms")
            .iter()
            .all(|r| r.path.contains("/v4.3/")),
        "and routes at the version it named"
    );
    let users = nutsh_catalog::kind("iam.authn.User").expect("users");
    let _ = client.list_page(users, 0, &ListOptions::default()).await;
    assert!(
        pc.requests().iter().all(|r| !r.path.contains("v9.9")),
        "a version iam does not offer is dropped, not routed to: {:?}",
        pc.requests()
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>()
    );
}

/// A restored pin that is wrong costs one namespace, once: the list that 404s triggers
/// `negotiate_namespace` for that namespace alone.
#[tokio::test]
async fn a_404_under_an_adopted_pin_repairs_that_namespace() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.3"])
        .start()
        .await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    // A pin at a version this Prism Central does not serve.
    client.adopt_pins(&pins(&[("vmm", "v4.0")]), vec![restored("vmm", "v4.0")]);
    let first = client.list_page(vms(), 0, &ListOptions::default()).await;
    assert!(first.is_err(), "the wrong pin 404s");
    // The repair ran behind it, so the next cycle routes to the version that answers.
    assert_eq!(
        client
            .statuses()
            .iter()
            .find(|s| s.name == "vmm")
            .and_then(|s| s.pinned),
        Some("v4.3"),
        "the pin was repaired for that namespace alone"
    );
    assert!(
        client
            .list_page(vms(), 0, &ListOptions::default())
            .await
            .is_ok()
    );
    // Repaired means probed, so a later failure is no longer this namespace's to re-probe.
    let before = client.metrics().started();
    let _ = client.list_page(vms(), 0, &ListOptions::default()).await;
    assert_eq!(
        client.metrics().started() - before,
        1,
        "one list, and no probe behind it"
    );
}

/// The 60 s window, where it is the only thing that can apply: the repair's own probe fails
/// with a host-wide verdict, so the namespace stays adopted and the `adopted` check cannot be
/// what stops the second failing list. A failing service must not be turned into a
/// re-negotiation storm.
#[tokio::test]
async fn a_second_failure_inside_the_window_does_not_probe_again() {
    let pc = MockPc::builder()
        // The list under the adopted pin: down, which is a reason to repair.
        .fail_path("/vmm/v4.0/ahv/config/vms", 500)
        // The first candidate the repair probes: a 429 is host-wide, so the probe aborts and
        // settles nothing, which leaves vmm adopted.
        //
        // Not a 401, though that is host-wide too: a 401 on a request that carried the
        // credential ends the session outright, so the second list below would issue no request
        // at all and this test could not tell the repair window from a stopped client.
        // `a_401_during_a_repair_ends_the_session` pins that behaviour instead.
        .fail_path("/vmm/v4.3/content/ovas", 429)
        .start()
        .await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    client.adopt_pins(&pins(&[("vmm", "v4.0")]), vec![restored("vmm", "v4.0")]);

    assert!(
        client
            .list_page(vms(), 0, &ListOptions::default())
            .await
            .is_err(),
        "the pinned version is down"
    );
    let probes = pc.requests_to("/content/ovas").len();
    assert!(probes > 0, "the repair probed");
    assert_eq!(
        client
            .statuses()
            .iter()
            .find(|s| s.name == "vmm")
            .and_then(|s| s.pinned),
        Some("v4.0"),
        "and settled nothing, so vmm is still adopted"
    );

    let before = client.metrics().started();
    assert!(
        client
            .list_page(vms(), 0, &ListOptions::default())
            .await
            .is_err()
    );
    assert_eq!(
        client.metrics().started() - before,
        1,
        "one list, and no second probe sweep"
    );
    assert_eq!(
        pc.requests_to("/content/ovas").len(),
        probes,
        "the window, not the adopted set, is what held it back"
    );
}

/// The lazy repair reaches the network like anything else, so a 401 there is a credential this
/// Prism Central refused - and the session is over. Nothing is probed again and nothing is
/// listed again, because there is nothing left that could be answered.
#[tokio::test]
async fn a_401_during_a_repair_ends_the_session() {
    let pc = MockPc::builder()
        .fail_path("/vmm/v4.0/ahv/config/vms", 500)
        .fail_path("/vmm/v4.3/content/ovas", 401)
        .start()
        .await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    client.adopt_pins(&pins(&[("vmm", "v4.0")]), vec![restored("vmm", "v4.0")]);

    assert!(
        client
            .list_page(vms(), 0, &ListOptions::default())
            .await
            .is_err()
    );
    assert!(client.auth_rejected());

    let spent = client.metrics().started();
    assert!(matches!(
        client.list_page(vms(), 0, &ListOptions::default()).await,
        Err(PrismError::Auth)
    ));
    assert_eq!(
        client.metrics().started(),
        spent,
        "a refused credential is never presented again"
    );
}

/// The domain manager read `connect` already makes for the PC version now carries the extId
/// too, so the identity check costs nothing extra.
#[tokio::test]
async fn the_domain_manager_read_returns_the_ext_id_and_the_version() {
    let pc = MockPc::builder().start().await;
    let client = Client::connect(&profile(&pc), "secret").unwrap();
    client.negotiate().await.unwrap();
    let (ext_id, version) = client
        .pc_identity()
        .await
        .expect("the domain manager answers");
    assert!(!ext_id.is_empty());
    assert!(version.is_some_and(|v| v.starts_with("pc.")));
}

/// The session cookie belongs to one Prism Central and to no other host.
///
/// A followed `302 Location: http://elsewhere/` is a leak rather than a hop, because of the
/// layer order: `reqwest` builds `FollowRedirect::with_policy(CookieService::new(..))`,
/// so `tower-http` strips the `Cookie` header on the hop and the cookie layer immediately puts
/// it back for the new host - `NTNX_IAM_SESSION` to whatever the `Location` named. Prism's
/// `/api/*` has no legitimate redirect, so none is followed and a `302` comes back as the
/// answer it is.
#[tokio::test]
async fn a_redirect_is_not_followed_and_the_session_never_leaves_this_prism_central() {
    let elsewhere = MockPc::builder().start().await;
    let pc = MockPc::builder().start().await;
    let client = Client::connect(&profile(&pc), "secret").unwrap();
    // Twice: the first answer opens the session, the second rides it. There has to be a cookie
    // in the jar before a redirect can carry one away.
    for _ in 0..2 {
        client
            .list_page(vms(), 0, &ListOptions::default())
            .await
            .expect("the vms list");
    }
    assert!(
        pc.requests().iter().any(|r| r.header("cookie").is_some()),
        "the session is being ridden by now"
    );

    pc.redirect_from_now(
        "/vmm/v4.3/ahv/config/vms",
        &format!("http://{}:{}", elsewhere.host(), elsewhere.port()),
    );
    let e = client
        .list_page(vms(), 0, &ListOptions::default())
        .await
        .expect_err("a 302 is not a list");
    assert!(
        matches!(&e, PrismError::Api { status: 302, .. }),
        "a redirect is an answer, not a hop: {e}"
    );
    assert!(
        elsewhere.requests().is_empty(),
        "the other host was never asked at all, with or without a cookie: {:?}",
        elsewhere.requests()
    );
}
