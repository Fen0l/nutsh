//! The meter counts what went on the wire, not what a caller meant to send.

use std::sync::Arc;

use nutsh_mockpc::MockPc;
use nutsh_prism::{Client, ListOptions, Profile};

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

/// Every list page is one `started` and one `completed`, and a client that has issued nothing
/// says so - which is what the cache's "zero negotiation requests" assertion rests on.
#[tokio::test]
async fn a_list_counts_one_started_and_one_completed() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    assert_eq!(client.metrics().started(), 0, "nothing yet");
    let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
    client
        .list_page(vms, 0, &ListOptions::default())
        .await
        .unwrap();
    assert_eq!(client.metrics().started(), 1);
    assert_eq!(client.metrics().completed(), 1);
    assert!(client.metrics().mean_latency().is_some());
}

/// A 404 is a result: it completes, and it never colours the meter.
#[tokio::test]
async fn a_404_completes_and_does_not_count_as_a_failure() {
    let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
    let pc = MockPc::builder().missing_path(vms.list_path).start().await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    assert!(
        client
            .list_page(vms, 0, &ListOptions::default())
            .await
            .is_err()
    );
    assert_eq!(client.metrics().started(), 1);
    assert_eq!(client.metrics().completed(), 1);
    assert!(!client.metrics().snapshot(std::time::Instant::now()).failed);
}

/// A 429 costs the Prism Central twice, so it counts twice, and it drains a bucket, so the
/// meter goes amber.
#[tokio::test]
async fn a_429_counts_twice_and_shows_as_pacing() {
    let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
    let pc = MockPc::builder()
        .rate_limit_once(vms.list_path)
        .start()
        .await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    client
        .list_page(vms, 0, &ListOptions::default())
        .await
        .unwrap();
    assert_eq!(client.metrics().started(), 2, "the retry is a request too");
    assert_eq!(client.metrics().rate_limited(), 1);
    assert!(
        client
            .metrics()
            .snapshot(std::time::Instant::now())
            .throttled
    );
}

/// The red half of the meter, on the branch a 404 does not take: a 5xx is the far end failing
/// to answer, so it completes *and* it colours the meter.
#[tokio::test]
async fn a_5xx_completes_and_turns_the_meter_red() {
    let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
    let pc = MockPc::builder()
        .fail_path(vms.list_path, 503)
        .start()
        .await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    assert!(
        client
            .list_page(vms, 0, &ListOptions::default())
            .await
            .is_err()
    );
    assert_eq!(client.metrics().started(), 1);
    assert_eq!(client.metrics().completed(), 1, "a 5xx is still a response");
    assert!(client.metrics().snapshot(std::time::Instant::now()).failed);
}

/// The other half of the same branch. A 401 is not a row any view can draw - the credentials the
/// whole session runs on are wrong - so it belongs on the meter, where one empty table cannot
/// pass for a quiet Prism Central.
#[tokio::test]
async fn a_401_turns_the_meter_red() {
    let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
    let pc = MockPc::builder().start().await;
    let client = Arc::new(Client::connect(&profile(&pc), "nope").unwrap());
    assert!(
        client
            .list_page(vms, 0, &ListOptions::default())
            .await
            .is_err()
    );
    assert_eq!(client.metrics().completed(), 1);
    assert!(client.metrics().snapshot(std::time::Instant::now()).failed);
}

/// The third funnel, and the one no arithmetic test can reach: `acquire` sleeping on the local
/// budget. The VM read tier is two a second, so the third read inside one window is paced -
/// the precedent, and the half-second it costs, is `tests/actions.rs`'s budget test.
#[tokio::test]
async fn the_local_budget_pacing_a_read_shows_as_amber() {
    const WEB01: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
    let vms = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
    let pc = MockPc::builder().start().await;
    let client = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    for _ in 0..3 {
        client.get(vms, WEB01).await.unwrap();
    }
    assert_eq!(client.metrics().started(), 3, "no 429 and no retry here");
    assert!(
        client
            .metrics()
            .snapshot(std::time::Instant::now())
            .throttled,
        "the third read waited on the budget, and the meter says so"
    );
}
