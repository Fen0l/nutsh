use nutsh_catalog::{Kind, NAMESPACES, kind, namespace};
use nutsh_mockpc::MockPc;
use nutsh_prism::{Availability, Client, ListOptions, PrismError, Profile};

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

fn vm() -> &'static Kind {
    kind("vmm.ahv.config.Vm").unwrap()
}

#[tokio::test]
async fn default_mock_pins_every_namespace_at_its_catalog_version() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let statuses = c.negotiate().await.unwrap();
    assert_eq!(statuses.len(), NAMESPACES.len());
    for s in &statuses {
        assert!(s.ok, "{} {}", s.name, s.detail);
        assert_eq!(s.pinned, Some(s.version), "{}", s.name);
    }
    assert!(matches!(c.availability(vm()), Availability::Served));
}

#[tokio::test]
async fn steps_down_to_the_version_the_pc_serves_and_rewrites_every_url() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let statuses = c.negotiate().await.unwrap();
    let vmm = statuses.iter().find(|s| s.name == "vmm").unwrap();
    assert_eq!((vmm.pinned, vmm.ok), (Some("v4.1"), true), "{}", vmm.detail);
    assert!(vmm.detail.contains("/vmm/v4.1/"), "{}", vmm.detail);

    let negotiated = pc.requests().len();
    let page = c.list_page(vm(), 0, &ListOptions::default()).await.unwrap();
    assert_eq!(page.entities.len(), 3);
    let later: Vec<String> = pc
        .requests()
        .into_iter()
        .skip(negotiated)
        .map(|r| r.path)
        .collect();
    assert_eq!(later, vec!["/api/vmm/v4.1/ahv/config/vms"]);

    let web01 = c
        .get(vm(), "3d0c4a2e-1b8f-4c1a-9e2f-000000000001")
        .await
        .unwrap();
    assert_eq!(web01.name, "web-01");
    assert!(
        !pc.requests_to("/vmm/v4.1/ahv/config/vms/3d0c4a2e-1b8f-4c1a-9e2f-000000000001")
            .is_empty()
    );
}

/// `pinned_path_for` still sends a kind newer than the pin at its *own* version: a caller that
/// asks by name - a `get`, an action, a `--check` probe - gets the server's answer rather than
/// this client's opinion.
///
/// What changed, and why the two halves of this test now disagree: routing such a request is
/// right, *volunteering* one is not. `availability` refuses the kind, so nothing that greys a
/// row subscribes to it; the path below is what happens when something asks anyway.
#[tokio::test]
async fn a_kind_newer_than_the_pin_is_routed_at_its_own_version_but_never_volunteered() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    let profile_kind = kind("vmm.ahv.config.VmProfile").unwrap();
    assert_eq!(profile_kind.since, "v4.3");

    // Still sent at its own version, which is what `pinned_path_for` has always done.
    let negotiated = pc.requests().len();
    let result = c.list_page(profile_kind, 0, &ListOptions::default()).await;
    assert!(matches!(result, Err(PrismError::NotFound(_))), "{result:?}");
    let later: Vec<String> = pc
        .requests()
        .into_iter()
        .skip(negotiated)
        .map(|r| r.path)
        .collect();
    assert_eq!(later, vec!["/api/vmm/v4.3/ahv/config/vm-profiles"]);

    // And refused to anything that would have volunteered the request - which is the 404
    // above, once a cycle, for the whole session.
    assert_eq!(
        c.availability(profile_kind),
        Availability::NotServedAtPin {
            namespace: "vmm",
            since: profile_kind.since,
            pinned: "v4.1"
        }
    );
    assert!(!c.is_served(profile_kind));
    assert!(matches!(c.availability(vm()), Availability::Served));
    assert!(c.is_served(vm()));
}

/// `--check` reads `statuses()` and never `availability`, so what a namespace is pinned at is
/// still reported exactly as it was - which is the spec's "`NewerThanPc` survives for the case
/// `--check` already knows", asserted rather than kept as an unreachable variant.
#[tokio::test]
async fn check_still_reports_the_pinned_version_per_namespace() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    let statuses = c.statuses();
    let vmm = statuses.iter().find(|s| s.name == "vmm").unwrap();
    assert!(vmm.ok);
    assert_eq!(vmm.pinned, Some("v4.1"));
    assert_eq!(vmm.version, "v4.3", "what the catalog was generated from");
}

#[tokio::test]
async fn an_unavailable_namespace_is_one_failed_row() {
    let pc = MockPc::builder()
        .unavailable_namespace("files")
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let statuses = c.negotiate().await.unwrap();
    assert_eq!(statuses.len(), NAMESPACES.len());
    let files = statuses.iter().find(|s| s.name == "files").unwrap();
    assert_eq!((files.ok, files.pinned), (false, None));
    assert!(
        files.detail.starts_with("not served at"),
        "{}",
        files.detail
    );
    for s in statuses.iter().filter(|s| s.name != "files") {
        assert!(s.ok, "{} {}", s.name, s.detail);
    }
}

/// And the one case that is still a refusal without a request: a namespace that answered at no
/// version at all is the only thing `availability` and `is_served` refuse on their own.
#[tokio::test]
async fn a_namespace_served_at_no_version_lists_what_was_tried() {
    let pc = MockPc::builder().serve_versions("vmm", &[]).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let statuses = c.negotiate().await.unwrap();
    let vmm = statuses.iter().find(|s| s.name == "vmm").unwrap();
    assert_eq!((vmm.pinned, vmm.ok), (None, false));
    assert_eq!(
        vmm.detail,
        format!(
            "not served at {}",
            namespace("vmm").unwrap().versions.join(", ")
        )
    );
    assert!(matches!(
        c.availability(vm()),
        Availability::NamespaceUnavailable(ref d) if d.starts_with("not served")
    ));
    assert!(!c.is_served(vm()));
}

#[tokio::test]
async fn a_forbidden_namespace_is_served() {
    let pc = MockPc::builder().forbid_namespace("vmm").start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let statuses = c.negotiate().await.unwrap();
    let vmm = statuses.iter().find(|s| s.name == "vmm").unwrap();
    assert_eq!((vmm.pinned, vmm.ok), (Some("v4.3"), true));
    assert!(vmm.detail.contains("not permitted"), "{}", vmm.detail);
}

#[tokio::test]
async fn bad_credentials_abort_negotiation() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "nope").unwrap();
    assert!(matches!(c.negotiate().await, Err(PrismError::Auth)));
}

#[tokio::test]
async fn before_negotiation_nothing_is_available() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    assert!(matches!(
        c.availability(vm()),
        Availability::NamespaceUnavailable(ref d) if d == "not negotiated"
    ));
}

#[tokio::test]
async fn negotiate_namespace_pins_one_namespace_only() {
    let pc = MockPc::builder()
        .serve_versions("clustermgmt", &["v4.0"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let status = c.negotiate_namespace("clustermgmt").await.unwrap();
    assert_eq!((status.pinned, status.ok), (Some("v4.0"), true));
    let clusters = c
        .list_page(
            kind("clustermgmt.config.Cluster").unwrap(),
            0,
            &ListOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(clusters.entities.len(), 1);
    assert!(
        !pc.requests_to("/clustermgmt/v4.0/config/clusters")
            .is_empty()
    );
    assert!(matches!(
        c.availability(vm()),
        Availability::NamespaceUnavailable(ref d) if d == "not negotiated"
    ));
    assert!(matches!(
        c.negotiate_namespace("nope").await,
        Err(PrismError::Catalog(_))
    ));
}

#[tokio::test]
async fn the_pc_version_comes_from_the_domain_manager_or_is_none() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    let version = c.pc_identity().await.and_then(|(_, v)| v);
    assert_eq!(version.as_deref(), Some("pc.2024.3"));

    // No domain manager endpoint at all: the header shows no version, and a 404 is still an
    // answer, so `pc_identity_result` reports it rather than the session failing over it.
    let pc = MockPc::builder()
        .unavailable_namespace("prism")
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    assert!(matches!(
        c.pc_identity_result().await,
        Err(PrismError::NotFound(_))
    ));
    assert_eq!(c.pc_identity().await, None);
}

/// Requests whose path starts with `prefix`; `requests_to` matches suffixes only.
fn requests_under(pc: &MockPc, prefix: &str) -> usize {
    pc.requests()
        .iter()
        .filter(|r| r.path.starts_with(prefix))
        .count()
}

/// The version segments of the vmm probes, in arrival order, collapsed to one entry per run.
fn vmm_probe_versions(pc: &MockPc) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for r in pc.requests() {
        let Some(api_path) = r.path.strip_prefix("/api") else {
            continue;
        };
        if !api_path.starts_with("/vmm/") {
            continue;
        }
        let Some(v) = nutsh_catalog::version_in(api_path) else {
            continue;
        };
        if seen.last().map(String::as_str) != Some(v) {
            seen.push(v.to_string());
        }
    }
    seen
}

#[tokio::test]
async fn probes_newest_version_first() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    assert_eq!(vmm_probe_versions(&pc), vec!["v4.3", "v4.2", "v4.1"]);
}

#[tokio::test]
async fn a_probe_is_never_redirected_by_an_earlier_pin() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    assert_eq!(
        c.negotiate_namespace("vmm").await.unwrap().pinned,
        Some("v4.1")
    );
    let newest_before = requests_under(&pc, "/api/vmm/v4.3/");
    assert!(newest_before > 0);

    let statuses = c.negotiate().await.unwrap();
    let vmm = statuses.iter().find(|s| s.name == "vmm").unwrap();
    assert_eq!((vmm.pinned, vmm.ok), (Some("v4.1"), true), "{}", vmm.detail);
    assert!(
        requests_under(&pc, "/api/vmm/v4.3/") > newest_before,
        "the second negotiation must probe v4.3 again, not follow the v4.1 pin"
    );
}

#[tokio::test]
async fn a_failing_namespace_is_reported_down_at_its_version() {
    let pc = MockPc::builder()
        .fail_namespace("opsmgmt", 503)
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let statuses = c.negotiate().await.unwrap();
    let ops = statuses.iter().find(|s| s.name == "opsmgmt").unwrap();
    assert_eq!((ops.pinned, ops.ok), (Some("v4.0"), false));
    // The row names the endpoint that failed, not just the failure.
    assert!(ops.detail.contains("/opsmgmt/v4.0/"), "{}", ops.detail);
    assert!(ops.detail.contains("HTTP 503"), "{}", ops.detail);
    match c.availability(kind("opsmgmt.config.Report").unwrap()) {
        Availability::NamespaceUnavailable(d) => assert_eq!(d, ops.detail),
        other => panic!("expected NamespaceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn a_served_namespace_that_lacks_one_kind_is_still_pinned() {
    // The kind the client probes first, chosen exactly as `probe_candidates` does.
    let mut candidates: Vec<&'static Kind> = nutsh_catalog::KINDS
        .iter()
        .filter(|k| {
            k.namespace == "clustermgmt"
                && k.is_top_level()
                && k.list_params.limit
                && !k.list_params.required
                && k.served_at("v4.3")
        })
        .collect();
    candidates.sort_by_key(|k| (k.list_path.len(), k.list_path));
    let first = candidates[0].list_path;

    let pc = MockPc::builder().missing_path(first).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let statuses = c.negotiate().await.unwrap();
    let cm = statuses.iter().find(|s| s.name == "clustermgmt").unwrap();
    assert_eq!((cm.pinned, cm.ok), (Some("v4.3"), true), "{}", cm.detail);
    // The 404 moved the probe on to the next candidate rather than condemning the namespace.
    assert!(!cm.detail.contains(first), "{}", cm.detail);
    assert!(cm.detail.contains(candidates[1].list_path), "{}", cm.detail);
}

#[tokio::test]
async fn rate_limiting_during_negotiation_aborts() {
    // Not `rate_limit_once`: that fires once and the client's own retry would swallow it.
    let pc = MockPc::builder().fail_namespace("vmm", 429).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    assert!(matches!(
        c.negotiate().await,
        Err(PrismError::RateLimited { .. })
    ));
}

/// A host nothing listens on: the first refused connection ends negotiation and the probes
/// still in flight are dropped. A refused connection fails immediately, so the point of the
/// clock is that twenty of them are not serialised behind twenty connect timeouts.
#[tokio::test]
async fn a_dead_host_fails_once_and_quickly() {
    // Bind to learn a port the OS has free, then drop the listener so nothing accepts on it.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let profile = Profile {
        host: "127.0.0.1".into(),
        port,
        username: "admin".into(),
        verify_tls: true,
        ca_bundle: None,
        plain_http: true,
    };
    let c = Client::connect(&profile, "secret").unwrap();
    let started = std::time::Instant::now();
    let result = c.negotiate().await;
    let elapsed = started.elapsed();
    match &result {
        Err(PrismError::Connect { host, .. }) => assert_eq!(host, "127.0.0.1"),
        other => panic!("expected one Connect error, got {other:?}"),
    }
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "negotiation took {elapsed:?}: the probes are not being dropped on the first failure"
    );
}

/// The other refusal that costs no request, and the one the Disaster Recovery page was paying
/// a 404 a cycle for: the namespace answers, but not at a version that has this kind.
/// `multidomain` pinned at v4.2 is the recording exactly - `RegisteredDomain` first appears at v4.3,
/// and `/multidomain/v4.3/config/registered-domains` is not a path this Prism Central routes.
#[tokio::test]
async fn a_kind_newer_than_the_pin_is_refused_at_no_cost() {
    let pc = MockPc::builder()
        .serve_versions("multidomain", &["v4.2"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    let registered = kind("multidomain.config.RegisteredDomain").unwrap();
    assert_eq!(registered.since, "v4.3", "the premise of the test");
    assert_eq!(
        c.availability(registered),
        Availability::NotServedAtPin {
            namespace: "multidomain",
            since: "v4.3",
            pinned: "v4.2"
        }
    );
    assert_eq!(
        c.availability(registered).reason().as_deref(),
        Some("needs multidomain v4.3 (this PC pinned v4.2)")
    );
    assert!(!c.is_served(registered));
    // The namespace itself is up, and a kind that is in v4.2 is served: the verdict is the
    // kind's version and nothing wider.
    let repositories = kind("multidomain.config.ExternalRepository").unwrap();
    assert_eq!(repositories.since, "v4.2");
    assert_eq!(c.availability(repositories), Availability::Served);
    assert_eq!(
        c.namespace_availability("multidomain"),
        Availability::Served,
        "the two questions are separate, and this one is about the namespace"
    );
}

/// The refusal is a prediction the catalog makes, and `pinned_path_for` already argues that a
/// Prism Central may route a newer path anyway. `ask_anyway` is where a person overrules the
/// prediction: one request, at the kind's own version, and whatever comes back stands.
#[tokio::test]
async fn ask_anyway_lets_the_server_answer_for_a_kind_newer_than_the_pin() {
    let pc = MockPc::builder()
        .serve_versions("multidomain", &["v4.2"])
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    let registered = kind("multidomain.config.RegisteredDomain").unwrap();
    assert!(!c.is_served(registered), "greyed to begin with");
    c.ask_anyway(registered);
    assert_eq!(
        c.availability(registered),
        Availability::Served,
        "the prediction is set aside for this kind alone"
    );
    let other = kind("multidomain.config.ExternalRepository").unwrap();
    assert_eq!(c.availability(other), Availability::Served);
    // And the request goes to the kind's own version, which is where the catalog says the path
    // is. This mock does not route it, so the 404 is the answer, and it is remembered: the
    // kind greys again, now on what the server said rather than on what the catalog predicted.
    let e = c
        .list_page(registered, 0, &ListOptions::default())
        .await
        .expect_err("v4.3 is not routed here");
    assert!(matches!(e, PrismError::NotFound(_)), "{e:?}");
    assert!(
        pc.requests().iter().any(|r| r
            .path
            .contains("/multidomain/v4.3/config/registered-domains")),
        "asked at the kind's own version: {:?}",
        pc.requests().iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    c.mark_missing(registered);
    assert_eq!(
        c.list_availability(registered).reason().as_deref(),
        Some(nutsh_prism::NOT_SERVED),
        "the server's answer replaces the catalog's guess"
    );
}
