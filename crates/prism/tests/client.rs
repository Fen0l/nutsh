use nutsh_catalog::{Kind, kind};
use nutsh_mockpc::MockPc;
use nutsh_prism::{Client, ListOptions, PrismError, Profile};

const WEB01: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";

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
async fn list_all_walks_pages_in_order() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let vms = c
        .list_all(
            vm(),
            &ListOptions {
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let names: Vec<&str> = vms.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["web-01", "web-02", "db-01"]);
    let reqs = pc.requests_to("/ahv/config/vms");
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs[1]
            .query
            .contains(&("$page".to_string(), "1".to_string()))
    );
    assert!(
        reqs[1]
            .query
            .contains(&("$limit".to_string(), "2".to_string()))
    );
    assert_eq!(reqs[0].header("accept"), Some("application/json"));
}

/// A total this client cannot walk is refused, not attempted.
///
/// The `Some(total)` arm fanned the remaining pages out with no ceiling on how many, while the
/// total-less arm eleven lines below has had exactly that guard all along. A
/// `totalAvailableResults` of 2^63-1 yielded 2,061,584,303 pages - the truncating `as u32` did
/// not even fail loudly - and its one caller is the permission walk, so a Prism Central that
/// miscounts its IAM list turned that into an endless crawl at three requests a second with
/// nothing on the screen.
#[tokio::test]
async fn a_total_too_large_to_walk_is_refused_rather_than_fanned_out() {
    let pc = MockPc::builder()
        .miscount("/vmm/v4.3/ahv/config/vms", 9_223_372_036_854_775_807)
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let e = c
        .list_all(vm(), &ListOptions::default())
        .await
        .expect_err("a walk this long is not a walk");
    let message = e.to_string();
    assert!(
        matches!(e, PrismError::Decode(_)) && message.contains("9223372036854775807"),
        "it says what it was told: {message}"
    );
    assert_eq!(
        pc.requests_to("/ahv/config/vms").len(),
        1,
        "and it stopped at the page that told it, rather than asking for the second"
    );
}

#[tokio::test]
async fn get_carries_the_etag_header() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let e = c.get(vm(), WEB01).await.unwrap();
    assert_eq!(e.name, "web-01");
    assert_eq!(e.raw["powerState"], "ON");
    assert!(e.etag.as_deref().unwrap().starts_with('"'));
}

#[tokio::test]
async fn act_sends_request_id_and_if_match_and_no_body() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let task = match c.act(vm(), &[], WEB01, "power-on", None).await.unwrap() {
        nutsh_prism::ActionResult::Task(t) => t,
        other => panic!("{other:?}"),
    };
    assert!(task.ext_id.starts_with("ZXJnb24=:"));
    let reqs = pc.requests_to("/$actions/power-on");
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert!(
        reqs[0]
            .header("ntnx-request-id")
            .is_some_and(|v| v.len() == 36)
    );
    assert!(reqs[0].header("if-match").is_some());
    assert_eq!(reqs[0].body, None);
}

#[tokio::test]
async fn act_refuses_missing_body_and_unknown_action() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    assert!(matches!(
        // `guest-shutdown` needs a body; `clone` merely takes one, so `None` is fine there.
        c.act(vm(), &[], WEB01, "guest-shutdown", None).await,
        Err(PrismError::Catalog(_))
    ));
    assert!(matches!(
        c.act(vm(), &[], WEB01, "teleport", None).await,
        Err(PrismError::Catalog(_))
    ));
    assert!(pc.requests_to("/$actions/guest-shutdown").is_empty());
}

#[tokio::test]
async fn wrong_password_is_an_auth_error() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "nope").unwrap();
    assert!(matches!(
        c.list_page(vm(), 0, &ListOptions::default()).await,
        Err(PrismError::Auth)
    ));
}

#[tokio::test]
async fn missing_entity_is_not_found_with_message() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    match c.get(vm(), "does-not-exist").await {
        Err(PrismError::NotFound(msg)) => assert!(msg.contains("does-not-exist"), "{msg}"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn rate_limit_is_retried_once() {
    let pc = MockPc::builder()
        .rate_limit_once("/vmm/v4.3/ahv/config/vms")
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let page = c.list_page(vm(), 0, &ListOptions::default()).await.unwrap();
    assert_eq!(page.entities.len(), 3);
    assert_eq!(pc.requests_to("/ahv/config/vms").len(), 2);
}

#[tokio::test]
async fn resolve_cluster_exact_and_case_insensitive() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let r = c.resolve_cluster("lab-cluster").await.unwrap();
    assert_eq!(r.ext_id, "0006158a-2f0d-4d5a-8e2d-000000000010");
    assert_eq!(r.name, "lab-cluster");
    assert_eq!(
        c.resolve_cluster("LAB-CLUSTER").await.unwrap().ext_id,
        r.ext_id
    );
}

#[tokio::test]
async fn resolve_cluster_missing_lists_candidates() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    match c.resolve_cluster("nope").await {
        Err(PrismError::Unresolved(m)) => {
            assert!(m.contains("lab-cluster"), "{m}");
            // A name that matched nothing is not an HTTP status: the message must not claim one.
            assert!(!m.contains("HTTP"), "{m}");
        }
        other => panic!("expected Unresolved, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_cluster_ambiguous_lists_both() {
    let pc = MockPc::builder()
        .fixtures(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mockpc/fixtures-ambiguous"
        ))
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    match c.resolve_cluster("lab-cluster").await {
        Err(PrismError::Ambiguous(m)) => {
            assert!(
                m.contains("000000000010") && m.contains("000000000011"),
                "{m}"
            );
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[tokio::test]
async fn nested_kinds_list_under_their_parents() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let disks = kind("vmm.ahv.config.Disk").unwrap();
    let opts = ListOptions {
        parents: vec![WEB01.to_string()],
        ..Default::default()
    };
    let page = c.list_page(disks, 0, &opts).await.unwrap();
    assert_eq!(page.entities.len(), 2);
    assert!(
        !pc.requests_to(&format!("/vmm/v4.3/ahv/config/vms/{WEB01}/disks"))
            .is_empty()
    );

    let disk = c
        .get_in(
            disks,
            &[WEB01.to_string()],
            "9a1b2c3d-0000-4000-8000-000000000101",
        )
        .await
        .unwrap();
    assert_eq!(disk.raw["diskAddress"]["index"], 0);

    let nics = kind("clustermgmt.config.HostNic~host-nics").unwrap();
    let opts = ListOptions {
        parents: vec![
            "0006158a-2f0d-4d5a-8e2d-000000000010".into(),
            "7b2f2f70-0f6a-4b58-9f9b-000000000020".into(),
        ],
        ..Default::default()
    };
    assert_eq!(c.list_page(nics, 0, &opts).await.unwrap().entities.len(), 2);
}

#[tokio::test]
async fn wrong_parent_count_is_a_catalog_error() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let disks = kind("vmm.ahv.config.Disk").unwrap();
    let err = c
        .list_page(disks, 0, &ListOptions::default())
        .await
        .unwrap_err();
    match err {
        PrismError::Catalog(m) => {
            assert!(
                m.contains("vmm.ahv.config.Disk") && m.contains("1") && m.contains("0"),
                "{m}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert!(pc.requests().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn nested_list_all_forwards_parents() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let disks = kind("vmm.ahv.config.Disk").unwrap();
    let opts = ListOptions {
        limit: 1,
        parents: vec![WEB01.to_string()],
        ..Default::default()
    };
    let all = c.list_all(disks, &opts).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(
        pc.requests_to(&format!("/vmm/v4.3/ahv/config/vms/{WEB01}/disks"))
            .len(),
        2
    );
}

#[tokio::test]
async fn get_in_carries_the_etag() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let disks = kind("vmm.ahv.config.Disk").unwrap();
    let disk = c
        .get_in(
            disks,
            &[WEB01.to_string()],
            "9a1b2c3d-0000-4000-8000-000000000101",
        )
        .await
        .unwrap();
    assert!(disk.etag.is_some());
}

/// A GET at a path the catalog does not name: the one thing that needs it is the DR page's
/// sampler, whose kind has no list endpoint and is therefore absent from the catalog.
#[tokio::test]
async fn get_path_reads_an_entity_the_catalog_does_not_name() {
    let pc = MockPc::builder().start().await;
    let client = Client::connect(&profile(&pc), "secret").unwrap();
    let value = client
        .get_path(&format!(
            "/dataprotection/v4.4/config/protected-resources/{WEB01}"
        ))
        .await
        .unwrap();
    assert_eq!(
        value
            .pointer("/replicationStates/0/replicationStatus")
            .and_then(|v| v.as_str()),
        Some("IN_SYNC")
    );
    let missing = client
        .get_path("/dataprotection/v4.4/config/protected-resources/nope")
        .await;
    assert!(
        matches!(missing, Err(PrismError::NotFound(_))),
        "{missing:?}"
    );
    // A path the catalog does not name still spends the budgets every other request does: the
    // host ceiling, which is what the server actually meters, and the one synthetic bucket
    // these share, which has to exist for a 429 on it to drain anything.
    assert_eq!(client.host_debt(), 2, "both gets, the 404 included");
    assert_eq!(client.bucket_debt("GET", "GET (path)"), 2);
}

#[tokio::test]
async fn too_many_parents_is_a_catalog_error() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let opts = ListOptions {
        parents: vec!["x".to_string()],
        ..Default::default()
    };
    let err = c.list_page(vm(), 0, &opts).await.unwrap_err();
    assert!(matches!(err, PrismError::Catalog(_)));
    assert!(pc.requests().is_empty(), "nothing was sent");
}
