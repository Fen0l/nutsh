use nutsh_catalog::{Kind, kind};
use nutsh_mockpc::MockPc;
use nutsh_prism::{ActionResult, Client, ListOptions, PrismError, Profile};
use serde_json::{Value, json};

const WEB01: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
const WEB02: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000002";
const CLUSTER: &str = "0006158a-2f0d-4d5a-8e2d-000000000010";
const HOST: &str = "7b2f2f70-0f6a-4b58-9f9b-000000000020";
const CONTAINER: &str = "5f6e7d8c-0000-4000-8000-000000000201";

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
async fn act_fills_a_two_placeholder_path_from_the_parents() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let hosts = kind("clustermgmt.config.Host~hosts").unwrap();
    let r = c
        .act(
            hosts,
            &[CLUSTER.to_string()],
            HOST,
            "enter-host-maintenance",
            Some(json!({"timeoutSeconds": 60})),
        )
        .await
        .unwrap();
    assert!(matches!(r, ActionResult::Task(_)));
    let reqs = pc.requests_to("/$actions/enter-host-maintenance");
    assert_eq!(reqs.len(), 1);
    assert!(
        reqs[0]
            .path
            .contains(&format!("/clusters/{CLUSTER}/hosts/{HOST}/"))
    );
    assert_eq!(
        reqs[0].header("if-match"),
        None,
        "the overlay says this action declares no If-Match"
    );
}

#[tokio::test]
async fn a_count_mismatch_is_a_catalog_error_naming_both_counts() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let hosts = kind("clustermgmt.config.Host~hosts").unwrap();
    let err = c
        .act(hosts, &[], HOST, "enter-host-maintenance", Some(json!({})))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, PrismError::Catalog(_)), "{text}");
    // The whole message, not two digits that a version bump in the path could satisfy on its own.
    assert!(
        text.contains("enter-host-maintenance on clustermgmt.config.Host~hosts:"),
        "{text}"
    );
    assert!(text.contains("needs 2 path value(s), got 1"), "{text}");
}

#[tokio::test]
async fn the_two_body_refusals_and_the_etag_fetch() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    // power-off takes no body at all: an empty one is an HTTP 400 on a real PC.
    let err = c
        .act(vm(), &[], WEB01, "power-off", Some(json!({})))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("takes no request body"), "{err}");
    // clone declares a body without `required`, so a bodyless clone is allowed through.
    assert!(c.act(vm(), &[], WEB01, "clone", None).await.is_ok());
    // guest-shutdown declares one that is required.
    let err = c
        .act(vm(), &[], WEB01, "guest-shutdown", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("requires a request body"), "{err}");

    let etagged = pc.requests_to("/$actions/power-off");
    assert!(etagged.is_empty(), "the refusal never reached the wire");
    c.act(vm(), &[], WEB01, "power-off", None).await.unwrap();
    let sent = pc.requests_to("/$actions/power-off");
    let tag = sent[0].header("if-match").expect("If-Match");
    assert!(tag.starts_with('"'), "the weak prefix is stripped: {tag}");
}

#[tokio::test]
async fn returns_chooses_the_result_and_a_wrong_guess_degrades() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let task = match c.act(vm(), &[], WEB01, "power-off", None).await.unwrap() {
        ActionResult::Task(t) => t,
        other => panic!("{other:?}"),
    };
    let tasks = kind("prism.config.Task").unwrap();
    // `cancel` is `Payload`: its 200 carries an AppMessage, not a task.
    let r = c
        .act(tasks, &[], &task.ext_id, "cancel", None)
        .await
        .unwrap();
    match r {
        ActionResult::Payload(v) => assert!(v.get("message").is_some(), "{v}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn update_strips_the_read_only_keys_and_keeps_the_rest() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.update(vm(), &[], WEB01, json!({"name": "web-01-renamed"}))
        .await
        .unwrap();
    let put = pc
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .expect("a PUT");
    let body = put.body.expect("a body");
    for stripped in [
        "$objectType",
        "$reserved",
        "$metadata",
        "extId",
        "links",
        "tenantId",
        "createdBy",
        "creationTime",
        "lastModifiedTime",
    ] {
        assert!(body.get(stripped).is_none(), "{stripped} survived: {body}");
    }
    assert_eq!(body["name"], "web-01-renamed");
    assert!(
        body.get("memorySizeBytes").is_some(),
        "the rest is sent back"
    );
    // Nested `$objectType` carries the polymorphic discriminator a v4 body needs.
    assert!(
        body["disks"][0]["backingInfo"]["$objectType"].is_string(),
        "{body}"
    );
}

#[tokio::test]
async fn update_sends_the_etag_of_the_read_it_built_the_body_from() {
    let pc = MockPc::builder()
        .mutate_between_get_and_post("/vmm/v4.3/ahv/config/vms", WEB01)
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    // One GET, so the one-shot race hits it: the PUT carries that read's (now stale) ETag and
    // is refused. A second GET would have fetched the bumped ETag and overwritten the change.
    let err = c
        .update(vm(), &[], WEB01, json!({"name": "x"}))
        .await
        .unwrap_err();
    assert!(matches!(err, PrismError::Conflict { .. }), "{err}");
    assert_eq!(
        pc.requests().iter().filter(|r| r.method == "GET").count(),
        1,
        "the body and the If-Match come from the same read"
    );
}

#[tokio::test]
async fn update_refuses_changes_that_are_not_an_object() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let err = c
        .update(vm(), &[], WEB01, json!("web-01-renamed"))
        .await
        .unwrap_err();
    assert!(matches!(err, PrismError::Catalog(_)), "{err}");
    assert!(err.to_string().contains("must be a JSON object"), "{err}");
    assert!(pc.requests().is_empty(), "refused before any request");
}

#[tokio::test]
async fn update_cannot_smuggle_a_read_only_key_back_in() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.update(
        vm(),
        &[],
        WEB01,
        json!({"extId": "spoofed", "name": "kept"}),
    )
    .await
    .unwrap();
    let put = pc
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .expect("a PUT");
    let body = put.body.expect("a body");
    assert!(body.get("extId").is_none(), "{body}");
    assert_eq!(body["name"], "kept");
}

#[tokio::test]
async fn delete_is_act_on_the_delete_action() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    assert!(matches!(
        c.delete(vm(), &[], WEB01).await.unwrap(),
        ActionResult::Task(_)
    ));
    assert_eq!(
        pc.requests()
            .iter()
            .filter(|r| r.method == "DELETE")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_race_is_a_conflict_and_a_forbidden_action_is_forbidden() {
    let pc = MockPc::builder()
        .mutate_between_get_and_post("/vmm/v4.3/ahv/config/vms", WEB01)
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let err = c
        .act(vm(), &[], WEB01, "power-off", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PrismError::Conflict { .. }), "{err}");

    let pc = MockPc::builder()
        .forbid_action("vmm.ahv.config.Vm", "power-off")
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let err = c
        .act(vm(), &[], WEB01, "power-off", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PrismError::Forbidden(_)), "{err}");
}

#[tokio::test]
async fn a_storage_container_is_fetched_and_acted_on_by_container_ext_id() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let sc = kind("clustermgmt.config.StorageContainer").unwrap();
    let e = c.get(sc, CONTAINER).await.unwrap();
    assert_eq!(e.ext_id, CONTAINER, "read from containerExtId");
    let r = c
        .act(sc, &[], CONTAINER, "mount", Some(json!({"hostExtIds": []})))
        .await
        .unwrap();
    assert!(matches!(r, ActionResult::Task(_)));
    let sent: Vec<Value> = pc
        .requests_to("/$actions/mount")
        .into_iter()
        .filter_map(|r| r.body)
        .collect();
    assert_eq!(sent.len(), 1);
}

/// The bucket is paced against a caller-supplied clock, never a real sleep: a test that had to
/// wait a second per token would be the slowest test in the workspace.
#[test]
fn a_bucket_paces_requests_against_a_test_clock() {
    use std::time::{Duration, Instant};

    use nutsh_prism::bucket::Bucket;

    let t0 = Instant::now();
    let mut b = Bucket::new(nutsh_catalog::RateLimit {
        count: 2,
        per_secs: 1,
    });
    assert_eq!(b.take(t0), None, "first token");
    assert_eq!(b.take(t0), None, "second token");
    assert_eq!(
        b.take(t0),
        Some(Duration::from_secs(1)),
        "the budget is spent; wait a whole window"
    );
    assert_eq!(b.take(t0 + Duration::from_secs(1)), None, "refilled");
    // A 429 empties the bucket and holds it empty for what the server asked.
    b.drain(t0 + Duration::from_secs(1), Duration::from_secs(5));
    assert_eq!(
        b.take(t0 + Duration::from_secs(2)),
        Some(Duration::from_secs(4))
    );
    // A second 429 asking for less does not release the hold the first one set: the host
    // ceiling is drained by every template, and a `Retry-After: 1` there must not cut short a
    // minute another operation was told to wait.
    b.drain(t0 + Duration::from_secs(2), Duration::from_secs(1));
    assert_eq!(
        b.take(t0 + Duration::from_secs(3)),
        Some(Duration::from_secs(3)),
        "still the first deadline"
    );
    assert_eq!(b.take(t0 + Duration::from_secs(6)), None);
    // A peek reports the same wait and spends nothing: two peeks in a row, then the token is
    // still there to take.
    b.drain(t0 + Duration::from_secs(6), Duration::from_secs(2));
    assert_eq!(
        b.peek(t0 + Duration::from_secs(7)),
        Some(Duration::from_secs(1))
    );
    assert_eq!(
        b.peek(t0 + Duration::from_secs(7)),
        Some(Duration::from_secs(1))
    );
    assert_eq!(b.take(t0 + Duration::from_secs(8)), None);
}

#[test]
fn the_host_ceiling_caps_every_advertisement_at_thirty_a_second() {
    use std::time::Instant;

    use nutsh_prism::bucket::{Bucket, HOST_CEILING};

    let t0 = Instant::now();
    let mut b = Bucket::new(HOST_CEILING);
    for i in 0..30 {
        assert_eq!(b.take(t0), None, "token {i}");
    }
    assert!(
        b.take(t0).is_some(),
        "no advertisement may take this client past thirty a second"
    );
}

#[tokio::test]
async fn every_request_takes_a_token_from_its_path_template() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    // Two VMs, so two URLs: they still share one budget, which is what the server meters. Two
    // and not three because the ETag read is metered too, at the VM tier of two a second: a
    // third would spend a real second asleep and roll the window it is asserting on.
    for id in [WEB01, WEB02] {
        c.act(vm(), &[], id, "power-off", None).await.unwrap();
    }
    // Keyed by the template, not the URL.
    assert_eq!(
        c.bucket_debt(
            "POST",
            "/vmm/v4.3/ahv/config/vms/{extId}/$actions/power-off"
        ),
        2
    );
    // Every request takes a token: the two ETag reads spent the VM read budget, ...
    assert_eq!(c.bucket_debt("GET", "/vmm/v4.3/ahv/config/vms/{extId}"), 2);
    // ... and all four also spent one of the host ceiling.
    assert_eq!(c.host_debt(), 4);
}

/// One template, two operations, two budgets. The catalog declares the VM read at two a second
/// and the DELETE on the same path at five: a map keyed by the template alone would let
/// whichever call arrived first fix the tier for both - the write over its declared budget in
/// one order, every later read stalled under it in the other.
#[tokio::test]
async fn a_read_and_a_write_on_one_template_keep_their_own_budgets() {
    use std::time::{Duration, Instant};

    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    // `delete` needs an ETag, so this is one read and one DELETE.
    c.delete(vm(), &[], WEB02).await.unwrap();
    assert_eq!(
        c.bucket_debt("DELETE", "/vmm/v4.3/ahv/config/vms/{extId}"),
        1
    );
    assert_eq!(
        c.bucket_debt("GET", "/vmm/v4.3/ahv/config/vms/{extId}"),
        1,
        "the ETag read, and nothing the DELETE spent"
    );
    // And the read budget is still the kind's two a second, not the DELETE's five: the second
    // read fits in the window the ETag read opened, the third waits it out. The one real second
    // in this suite, and the only way to see a tier from outside the crate.
    let started = Instant::now();
    c.get(vm(), WEB01).await.unwrap();
    c.get(vm(), WEB01).await.unwrap();
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(500),
        "the third read went out inside the window: {waited:?}"
    );
}

/// The reactive half, end to end: the mock answers the first list with a 429 and `Retry-After:
/// 0`, so this costs no wall time and still exercises the template that rides through `send`,
/// the drain call site and the host ceiling.
#[tokio::test]
async fn a_429_drains_the_operation_and_the_host_ceiling() {
    use nutsh_prism::bucket::HOST_CEILING;

    let pc = MockPc::builder()
        .rate_limit_once("/vmm/v4.3/ahv/config/vms")
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.list_all(vm(), &ListOptions::default()).await.unwrap();
    assert_eq!(
        c.bucket_debt("GET", "/vmm/v4.3/ahv/config/vms"),
        vm().rate.count,
        "the 429 emptied the list budget, not just the one token the request spent"
    );
    assert_eq!(
        c.host_debt(),
        HOST_CEILING.count,
        "and the ceiling with it: the wall is the host's, not one template's"
    );
}
