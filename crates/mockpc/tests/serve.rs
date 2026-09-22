use std::time::Duration;

use nutsh_mockpc::{Gate, MockPc, Store};
use serde_json::{Value, json};

fn url(pc: &MockPc, path_and_query: &str) -> String {
    format!("http://{}:{}/api{}", pc.host(), pc.port(), path_and_query)
}

async fn get(pc: &MockPc, path: &str, password: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(url(pc, path))
        .basic_auth("admin", Some(password))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn lists_fixture_with_paging() {
    let pc = MockPc::builder().start().await;
    let r = get(&pc, "/vmm/v4.3/ahv/config/vms?$page=1&$limit=2", "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"][0]["name"], "db-01");
    assert_eq!(v["metadata"]["totalAvailableResults"], 3);
}

#[tokio::test]
async fn rejects_bad_password_with_error_envelope() {
    let pc = MockPc::builder().start().await;
    let r = get(&pc, "/vmm/v4.3/ahv/config/vms", "wrong").await;
    assert_eq!(r.status(), 401);
    let v: Value = r.json().await.unwrap();
    assert!(
        v["data"]["error"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Authentication")
    );
}

#[tokio::test]
async fn get_returns_etag_and_actions_require_it() {
    let pc = MockPc::builder().start().await;
    let id = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
    let r = get(&pc, &format!("/vmm/v4.3/ahv/config/vms/{id}"), "secret").await;
    assert_eq!(r.status(), 200);
    let etag = r
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(etag.starts_with('"'));
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"]["name"], "web-01");

    let client = reqwest::Client::new();
    let action = url(
        &pc,
        &format!("/vmm/v4.3/ahv/config/vms/{id}/$actions/power-on"),
    );
    let no_headers = client
        .post(&action)
        .basic_auth("admin", Some("secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(no_headers.status(), 400);
    let no_etag = client
        .post(&action)
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "r1")
        .send()
        .await
        .unwrap();
    assert_eq!(no_etag.status(), 400);
    let wrong_etag = client
        .post(&action)
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "r2")
        .header("If-Match", "\"stale\"")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_etag.status(), 412);
    let ok = client
        .post(&action)
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "r3")
        .header("If-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 202);
    let v: Value = ok.json().await.unwrap();
    assert!(
        v["data"]["extId"]
            .as_str()
            .unwrap()
            .starts_with("ZXJnb24=:")
    );
    let recorded = pc.requests_to("/$actions/power-on");
    assert_eq!(recorded.len(), 4);
    let last = recorded.last().unwrap();
    assert_eq!(last.header("NTNX-Request-Id"), Some("r3"));
    assert_eq!(last.header("if-match"), Some(etag.as_str()));
}

/// `files` is the namespace to ask this with: it is the one `store::version_of_reads_the_fixture_tree`
/// already pins as having no fixtures, so the two tests agree about which namespace is empty.
/// This was `/iam/v4.0/authn/users` until that path got a fixture of its own - the example has
/// to be a path nothing serves, so it must not be one a later task would want rows for.
#[tokio::test]
async fn known_catalog_path_without_fixture_is_an_empty_list() {
    let pc = MockPc::builder().start().await;
    let r = get(&pc, "/files/v4.0/config/file-servers", "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 0);
    assert_eq!(v["metadata"]["totalAvailableResults"], 0);
}

#[tokio::test]
async fn nested_list_without_a_fixture_is_an_empty_list() {
    let pc = MockPc::builder().start().await;
    let id = "3d0c4a2e-1b8f-4c1a-9e2f-000000000002";
    let r = get(
        &pc,
        &format!("/vmm/v4.3/ahv/config/vms/{id}/disks"),
        "secret",
    )
    .await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 0);
    assert_eq!(v["metadata"]["totalAvailableResults"], 0);

    let r = get(
        &pc,
        &format!("/vmm/v4.3/ahv/config/vms/{id}/nope"),
        "secret",
    )
    .await;
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn unknown_path_and_unavailable_namespace_are_404() {
    let pc = MockPc::builder()
        .unavailable_namespace("files")
        .start()
        .await;
    assert_eq!(get(&pc, "/nope/v1/things", "secret").await.status(), 404);
    assert_eq!(
        get(&pc, "/files/v4.0/config/file-servers", "secret")
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn rate_limit_once_then_serves() {
    let pc = MockPc::builder()
        .rate_limit_once("/vmm/v4.3/ahv/config/vms")
        .start()
        .await;
    let first = get(&pc, "/vmm/v4.3/ahv/config/vms", "secret").await;
    assert_eq!(first.status(), 429);
    assert_eq!(
        first
            .headers()
            .get("retry-after")
            .unwrap()
            .to_str()
            .unwrap(),
        "0"
    );
    assert_eq!(
        get(&pc, "/vmm/v4.3/ahv/config/vms", "secret")
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn limit_out_of_range_is_400() {
    let pc = MockPc::builder().start().await;
    for query in ["?$limit=101", "?$limit=0", "?$limit=abc"] {
        let r = get(&pc, &format!("/vmm/v4.3/ahv/config/vms{query}"), "secret").await;
        assert_eq!(r.status(), 400, "for {query}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(
            v["data"]["error"][0]["message"],
            "$limit must be between 1 and 100"
        );
    }
    let r = get(&pc, "/vmm/v4.3/ahv/config/vms?$page=nope", "secret").await;
    assert_eq!(r.status(), 400);
    assert_eq!(
        get(&pc, "/vmm/v4.3/ahv/config/vms?$limit=100", "secret")
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn power_on_rejects_a_body() {
    let pc = MockPc::builder().start().await;
    let id = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
    let etag = get(&pc, &format!("/vmm/v4.3/ahv/config/vms/{id}"), "secret")
        .await
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let client = reqwest::Client::new();
    let post = |path: String| {
        client
            .post(url(&pc, &path))
            .basic_auth("admin", Some("secret"))
            .header("NTNX-Request-Id", "r1")
            .header("If-Match", &etag)
    };

    // `power-on` is `needs_body: false`: even an empty object is rejected.
    let with_body = post(format!("/vmm/v4.3/ahv/config/vms/{id}/$actions/power-on"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(with_body.status(), 400);
    let v: Value = with_body.json().await.unwrap();
    assert_eq!(
        v["data"]["error"][0]["message"],
        "no body expected for this action"
    );

    // `guest-shutdown` is `needs_body: true`: omitting the body is rejected.
    let without_body = post(format!(
        "/vmm/v4.3/ahv/config/vms/{id}/$actions/guest-shutdown"
    ))
    .send()
    .await
    .unwrap();
    assert_eq!(without_body.status(), 400);
    let v: Value = without_body.json().await.unwrap();
    assert_eq!(v["data"]["error"][0]["message"], "request body required");

    // `clone` takes a body it does not need: omitting it is accepted.
    let optional = post(format!("/vmm/v4.3/ahv/config/vms/{id}/$actions/clone"))
        .send()
        .await
        .unwrap();
    assert_eq!(optional.status(), 202);

    // The store is unchanged: the ETag still matches.
    let after = get(&pc, &format!("/vmm/v4.3/ahv/config/vms/{id}"), "secret").await;
    assert_eq!(after.headers().get("etag").unwrap().to_str().unwrap(), etag);
}

#[tokio::test]
async fn serve_versions_maps_served_versions_onto_fixtures_and_refuses_the_rest() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.1", "v4.0"])
        .start()
        .await;
    // An older version is answered from the v4.3 fixture tree.
    let r = get(&pc, "/vmm/v4.1/ahv/config/vms?$limit=2", "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["metadata"]["totalAvailableResults"], 3);
    // An entity GET is mapped the same way.
    let id = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
    let r = get(&pc, &format!("/vmm/v4.1/ahv/config/vms/{id}"), "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"]["name"], "web-01");
    // The catalog's own version is not on the list: an old PC never had it.
    let r = get(&pc, "/vmm/v4.3/ahv/config/vms", "secret").await;
    assert_eq!(r.status(), 404);
    let v: Value = r.json().await.unwrap();
    let message = v["data"]["error"][0]["message"].as_str().unwrap();
    assert!(message.contains("v4.3"), "{message}");
    // Namespaces without a setting keep exact-path behaviour.
    let r = get(&pc, "/clustermgmt/v4.3/config/clusters", "secret").await;
    assert_eq!(r.status(), 200);
    let r = get(&pc, "/clustermgmt/v4.1/config/clusters", "secret").await;
    assert_eq!(r.status(), 404);
    // Requests are recorded as sent, not as mapped.
    let paths: Vec<String> = pc.requests().into_iter().map(|r| r.path).collect();
    assert!(
        paths.contains(&"/api/vmm/v4.1/ahv/config/vms".to_string()),
        "{paths:?}"
    );
}

#[tokio::test]
async fn serve_versions_with_no_fixture_falls_back_to_the_catalog_path() {
    // `files` has no fixtures and its catalog version is v4.0, so the request only succeeds
    // if the mapping falls back to the catalog's version.
    let pc = MockPc::builder()
        .serve_versions("files", &["v4.1"])
        .start()
        .await;
    let r = get(&pc, "/files/v4.1/config/file-servers", "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["metadata"]["totalAvailableResults"], 0);
}

#[tokio::test]
async fn serve_versions_with_an_empty_list_refuses_every_version() {
    let pc = MockPc::builder().serve_versions("vmm", &[]).start().await;
    assert_eq!(
        get(&pc, "/vmm/v4.3/ahv/config/vms", "secret")
            .await
            .status(),
        404
    );
    assert_eq!(
        get(&pc, "/vmm/v4.0/ahv/config/vms", "secret")
            .await
            .status(),
        404
    );
    let id = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
    assert_eq!(
        get(&pc, &format!("/vmm/v4.3/ahv/config/vms/{id}"), "secret")
            .await
            .status(),
        404
    );
    assert_eq!(
        get(&pc, "/clustermgmt/v4.3/config/clusters", "secret")
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn domain_manager_fixture_carries_the_pc_version() {
    let pc = MockPc::builder().start().await;
    let r = get(&pc, "/prism/v4.4/config/domain-managers?$limit=1", "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"][0]["config"]["buildInfo"]["version"], "pc.2024.3");
}

#[tokio::test]
async fn failing_namespaces_and_missing_paths() {
    let pc = MockPc::builder()
        .fail_namespace("opsmgmt", 503)
        .missing_path("/vmm/v4.3/ahv/config/vms")
        .start()
        .await;
    let r = get(&pc, "/opsmgmt/v4.0/config/reports", "secret").await;
    assert_eq!(r.status(), 503);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["data"]["error"][0]["message"],
        "opsmgmt is failing with HTTP 503"
    );

    let r = get(&pc, "/vmm/v4.3/ahv/config/vms", "secret").await;
    assert_eq!(r.status(), 404);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["data"]["error"][0]["message"],
        "no such path /vmm/v4.3/ahv/config/vms"
    );
    assert_eq!(
        get(&pc, "/clustermgmt/v4.3/config/clusters", "secret")
            .await
            .status(),
        200
    );
}

/// `missing_path` takes the path the client sends - at the version the namespace is pinned
/// to - and is consulted before `serve_versions` maps that version onto the fixtures. The
/// demo's second site pins `networking` at v4.0 and 404s the two v4.1-only kinds by that
/// spelling; keyed by the mapped path instead, the same string would answer 200-empty, because
/// a catalog-shaped path without a fixture is an empty list (`handler::get`'s last arm).
#[tokio::test]
async fn a_missing_path_is_keyed_by_the_clients_path_under_a_version_pin() {
    let pc = MockPc::builder()
        .serve_versions("networking", &["v4.0"])
        .missing_path("/networking/v4.0/config/nic-profiles")
        .start()
        .await;
    assert_eq!(
        get(&pc, "/networking/v4.0/config/nic-profiles", "secret")
            .await
            .status(),
        404
    );
    // The neighbouring kind at the same pin is the catalog-shaped empty list.
    let r = get(&pc, "/networking/v4.0/config/subnets", "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["metadata"]["totalAvailableResults"], 0);
    // And the version the fixtures carry is refused, which is what a pin means.
    assert_eq!(
        get(&pc, "/networking/v4.4/config/nic-profiles", "secret")
            .await
            .status(),
        404
    );
}

const VMS: &str = "/vmm/v4.3/ahv/config/vms";
const WEB01: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";

/// The catalog decides whether an action carries an `If-Match`, so this follows `needs_etag`
/// rather than pinning a header of its own.
async fn act(
    pc: &MockPc,
    kind_id: &str,
    action: &str,
    api_path: &str,
    body: Option<Value>,
) -> reqwest::Response {
    let a = nutsh_catalog::kind(kind_id)
        .and_then(|k| k.action(action))
        .unwrap_or_else(|| panic!("no {action} on {kind_id}"));
    let client = reqwest::Client::new();
    let mut req = client
        .post(url(pc, api_path))
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "11111111-2222-3333-4444-555555555555");
    if a.needs_etag {
        let entity_path = api_path.split("/$actions/").next().unwrap();
        req = req.header("If-Match", etag(pc, entity_path).await);
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    req.send().await.unwrap()
}

async fn etag(pc: &MockPc, entity_path: &str) -> String {
    get(pc, entity_path, "secret")
        .await
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

/// The task reference a 202 carries.
async fn task_id(r: reqwest::Response) -> String {
    r.json::<Value>().await.unwrap()["data"]["extId"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn task(pc: &MockPc, ext_id: &str) -> Value {
    let path = format!("/prism/v4.4/config/tasks/{ext_id}");
    get(pc, &path, "secret").await.json().await.unwrap()
}

#[tokio::test]
async fn a_mutation_creates_a_task_that_advances_one_step_per_get_and_flips_the_entity() {
    let pc = MockPc::builder().start().await;
    let r = act(
        &pc,
        "vmm.ahv.config.Vm",
        "power-off",
        &format!("{VMS}/{WEB01}/$actions/power-off"),
        None,
    )
    .await;
    assert_eq!(r.status(), 202);
    let v: Value = r.json().await.unwrap();
    let id = v["data"]["extId"].as_str().unwrap().to_string();
    assert!(id.starts_with("ZXJnb24=:"));

    // A list GET advances nothing, so a Tasks table never races the watcher.
    get(&pc, "/prism/v4.4/config/tasks", "secret").await;
    let queued = task(&pc, &id).await;
    assert_eq!(queued["data"]["status"], "QUEUED");
    assert!(
        queued["data"].get("startedTime").is_none(),
        "a queued task has not started"
    );
    let running = task(&pc, &id).await;
    assert_eq!(running["data"]["status"], "RUNNING");
    assert!(running["data"].get("startedTime").is_some());
    assert_ne!(
        running["data"]["lastUpdatedTime"], queued["data"]["lastUpdatedTime"],
        "every step moves lastUpdatedTime"
    );
    let third = task(&pc, &id).await;
    assert_eq!(third["data"]["status"], "RUNNING");
    assert_eq!(third["data"]["progressPercentage"], 50);
    let done = task(&pc, &id).await;
    assert_eq!(done["data"]["status"], "SUCCEEDED");
    assert_eq!(done["data"]["progressPercentage"], 100);
    assert_eq!(done["data"]["entitiesAffected"][0]["name"], "web-01");

    let vm: Value = get(&pc, &format!("{VMS}/{WEB01}"), "secret")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(vm["data"]["powerState"], "OFF", "the effect landed");
}

#[tokio::test]
async fn hooks_fail_stall_and_forbid_a_mutation() {
    let pc = MockPc::builder()
        .fail_task("power-on")
        .stall_task("reboot")
        .forbid_action("vmm.ahv.config.Vm", "delete")
        .start()
        .await;
    let id = task_id(
        act(
            &pc,
            "vmm.ahv.config.Vm",
            "power-on",
            &format!("{VMS}/{WEB01}/$actions/power-on"),
            None,
        )
        .await,
    )
    .await;
    // Three non-terminal steps, then the fourth GET is the terminal one, turned into a failure.
    for _ in 0..3 {
        assert_ne!(task(&pc, &id).await["data"]["status"], "FAILED");
    }
    let failed = task(&pc, &id).await;
    assert_eq!(failed["data"]["status"], "FAILED");
    assert_eq!(failed["data"]["progressPercentage"], 100);
    assert!(
        !failed["data"]["errorMessages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(task(&pc, &id).await["data"]["status"], "FAILED");
    let vm: Value = get(&pc, &format!("{VMS}/{WEB01}"), "secret")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        vm["data"]["powerState"], "ON",
        "a failed task applies no effect"
    );

    let stalled = task_id(
        act(
            &pc,
            "vmm.ahv.config.Vm",
            "reboot",
            &format!("{VMS}/{WEB01}/$actions/reboot"),
            None,
        )
        .await,
    )
    .await;
    for _ in 0..5 {
        assert_eq!(task(&pc, &stalled).await["data"]["status"], "QUEUED");
    }

    let forbidden = reqwest::Client::new()
        .delete(url(&pc, &format!("{VMS}/{WEB01}")))
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "x")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);
}

#[tokio::test]
async fn a_race_between_the_etag_fetch_and_the_mutation_is_a_412() {
    let pc = MockPc::builder()
        .mutate_between_get_and_post(VMS, WEB01)
        .start()
        .await;
    let r = act(
        &pc,
        "vmm.ahv.config.Vm",
        "power-off",
        &format!("{VMS}/{WEB01}/$actions/power-off"),
        None,
    )
    .await;
    assert_eq!(r.status(), 412);
    // The bump moved a timestamp the VM already carries rather than adding a field.
    let vm: Value = get(&pc, &format!("{VMS}/{WEB01}"), "secret")
        .await
        .json()
        .await
        .unwrap();
    assert!(vm["data"].get("$mockNonce").is_none());
    assert_eq!(vm["data"]["updateTime"], "2026-09-05T10:00:00Z");
    // One-shot: the next attempt succeeds.
    let r = act(
        &pc,
        "vmm.ahv.config.Vm",
        "power-off",
        &format!("{VMS}/{WEB01}/$actions/power-off"),
        None,
    )
    .await;
    assert_eq!(r.status(), 202);
}

#[tokio::test]
async fn a_delete_task_removes_the_row_once_it_succeeds() {
    let pc = MockPc::builder().start().await;
    let entity = format!("{VMS}/{WEB01}");
    let r = reqwest::Client::new()
        .delete(url(&pc, &entity))
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "d1")
        .header("If-Match", etag(&pc, &entity).await)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let id = task_id(r).await;
    // The row goes on the transition into SUCCEEDED, not at the 202.
    for _ in 0..3 {
        assert_ne!(task(&pc, &id).await["data"]["status"], "SUCCEEDED");
        assert_eq!(get(&pc, &entity, "secret").await.status(), 200);
    }
    let done = task(&pc, &id).await;
    assert_eq!(done["data"]["status"], "SUCCEEDED");
    assert_eq!(done["data"]["operation"], "delete");
    let list: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(list["metadata"]["totalAvailableResults"], 2);
    assert_eq!(get(&pc, &entity, "secret").await.status(), 404);
}

#[tokio::test]
async fn a_list_level_create_affects_no_entity() {
    let pc = MockPc::builder().start().await;
    let r = reqwest::Client::new()
        .post(url(&pc, VMS))
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "c1")
        .json(&serde_json::json!({"name": "new-vm"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let t = task(&pc, &task_id(r).await).await;
    assert_eq!(t["data"]["operation"], "create");
    assert_eq!(t["data"]["operationDescription"], "create");
    assert_eq!(t["data"]["numberOfEntitiesAffected"], 0);
    assert_eq!(t["data"]["entitiesAffected"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn task_steps_replaces_the_sequence_every_task_walks() {
    let pc = MockPc::builder()
        .task_steps(&[("RUNNING", 10), ("SUCCEEDED", 100)])
        .start()
        .await;
    let id = task_id(
        act(
            &pc,
            "vmm.ahv.config.Vm",
            "power-off",
            &format!("{VMS}/{WEB01}/$actions/power-off"),
            None,
        )
        .await,
    )
    .await;
    let first = task(&pc, &id).await;
    assert_eq!(first["data"]["status"], "RUNNING");
    assert_eq!(first["data"]["progressPercentage"], 10);
    let second = task(&pc, &id).await;
    assert_eq!(second["data"]["status"], "SUCCEEDED");
    assert_eq!(second["data"]["progressPercentage"], 100);
    let vm: Value = get(&pc, &format!("{VMS}/{WEB01}"), "secret")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(vm["data"]["powerState"], "OFF", "the effect landed");
}

/// Host maintenance is the one action family whose path (`/operations/clusters/{c}/hosts/{h}`)
/// differs from the entity's (`/config/clusters/{c}/hosts/{h}`): the mock looks the host up
/// under the config path. The catalog carries the action (its spec requires a body) and marks
/// it `needs_etag = false`; with no catalog action the mock's strict default for an entity
/// operation it cannot classify wants an `If-Match`, so the test follows the catalog for both
/// the header and the body, the way `act` does.
#[tokio::test]
async fn an_operations_action_finds_its_host_under_the_config_path() {
    let pc = MockPc::builder().start().await;
    let cluster = "0006158a-2f0d-4d5a-8e2d-000000000010";
    let host = "7b2f2f70-0f6a-4b58-9f9b-000000000020";
    let config = format!("/clustermgmt/v4.3/config/clusters/{cluster}/hosts/{host}");
    let action = format!(
        "/clustermgmt/v4.3/operations/clusters/{cluster}/hosts/{host}/$actions/enter-host-maintenance"
    );
    let catalog = nutsh_catalog::kind("clustermgmt.config.Host~hosts")
        .and_then(|k| k.action("enter-host-maintenance"));
    let wants_etag = catalog.is_none_or(|a| a.needs_etag);
    let wants_body = catalog.is_some_and(|a| a.needs_body);
    let mut req = reqwest::Client::new()
        .post(url(&pc, &action))
        .basic_auth("admin", Some("secret"))
        .header("NTNX-Request-Id", "m1");
    if wants_etag {
        req = req.header("If-Match", etag(&pc, &config).await);
    }
    if wants_body {
        req = req.json(&serde_json::json!({"shouldRollbackOnFailure": true}));
    }
    let r = req.send().await.unwrap();
    assert_eq!(r.status(), 202);
    let id = task_id(r).await;
    assert_eq!(
        task(&pc, &id).await["data"]["entitiesAffected"][0]["name"],
        "ahv-node-1"
    );
}

#[tokio::test]
async fn cancel_walks_canceling_then_canceled_and_refuses_a_terminal_task() {
    let pc = MockPc::builder().start().await;
    let id = task_id(
        act(
            &pc,
            "vmm.ahv.config.Vm",
            "power-off",
            &format!("{VMS}/{WEB01}/$actions/power-off"),
            None,
        )
        .await,
    )
    .await;
    let path = format!("/prism/v4.4/config/tasks/{id}/$actions/cancel");
    assert_eq!(
        act(&pc, "prism.config.Task", "cancel", &path, None)
            .await
            .status(),
        200
    );
    assert_eq!(task(&pc, &id).await["data"]["status"], "CANCELING");
    assert_eq!(task(&pc, &id).await["data"]["status"], "CANCELED");
    let again = act(&pc, "prism.config.Task", "cancel", &path, None).await;
    assert_eq!(again.status(), 400, "a terminal task is not cancelable");
}

/// The catalog says `manage-alert` needs both an `If-Match` and a body: the helper sends the
/// ETag, and the mock refuses the POST that has no body. The action is named `manage-alert` here
/// rather than `acknowledge` because that is the catalog's own name for it.
#[tokio::test]
async fn an_alert_action_needs_a_body_and_its_task_names_the_alert() {
    let pc = MockPc::builder().start().await;
    let path = "/monitoring/v4.3/serviceability/alerts/9ead870c-d76f-4475-8a13-000000000001";
    let action = format!("{path}/$actions/manage-alert");
    let r = act(
        &pc,
        "monitoring.serviceability.Alert",
        "manage-alert",
        &action,
        None,
    )
    .await;
    assert_eq!(r.status(), 400);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"]["error"][0]["message"], "request body required");

    let r = act(
        &pc,
        "monitoring.serviceability.Alert",
        "manage-alert",
        &action,
        Some(serde_json::json!({"actionType": "ACKNOWLEDGE"})),
    )
    .await;
    assert_eq!(r.status(), 202);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"]["$objectType"], "prism.v4.config.TaskReference");
    let id = v["data"]["extId"].as_str().unwrap();
    assert_eq!(
        task(&pc, id).await["data"]["entitiesAffected"][0]["name"],
        "Disk space usage high for /home on Controller VM 203.0.113.11"
    );
}

/// `repeat_fixture` invents nothing: a path with no fixture stays missing rather than growing a
/// list of made-up rows, and `n == 0` is the same no-op rather than a way to empty a real one.
#[tokio::test]
async fn repeat_fixture_leaves_a_path_without_a_fixture_and_a_zero_alone() {
    let pc = MockPc::builder()
        .repeat_fixture("/vmm/v4.3/ahv/config/nope", 10)
        .repeat_fixture(VMS, 0)
        .start()
        .await;
    assert_eq!(
        get(&pc, "/vmm/v4.3/ahv/config/nope", "secret")
            .await
            .status(),
        404
    );
    let r = get(&pc, VMS, "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["metadata"]["totalAvailableResults"], 3);
}

/// The gate is keyed on the path the client sent, before version mapping - which is why the
/// check sits between `rate_limit_once` and `served_path` in `handle`. Here the fixture lives at
/// v4.3 and the request is a v4.1 one, so holding the v4.1 path is the only thing that stops it.
#[tokio::test]
async fn hold_path_keys_the_clients_path_and_releases_one_answer() {
    let gate = Gate::new();
    let pc = MockPc::builder()
        .serve_versions("monitoring", &["v4.1"])
        .hold_path("/monitoring/v4.1/serviceability/audits", &gate)
        .start()
        .await;
    let held = url(&pc, "/monitoring/v4.1/serviceability/audits");
    let mut pending = tokio::spawn(async move {
        reqwest::Client::new()
            .get(held)
            .basic_auth("admin", Some("secret"))
            .send()
            .await
            .unwrap()
    });
    // Recorded before the gate, so a request still waiting is already counted. Waited for rather
    // than assumed: the spawned task has a client to build and a socket to open, and a loaded
    // runner is slow at both. Nothing here races the hold - a request that arrives is held.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while pc.requests_to("/serviceability/audits").is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the request never reached the mock"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Now the hold itself, which a slow machine only makes truer.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut pending)
            .await
            .is_err(),
        "the gate holds the answer"
    );
    assert_eq!(
        pc.requests_to("/serviceability/audits").len(),
        1,
        "one request, still waiting"
    );

    // Exactly this path: an entity GET underneath it is another path, and is answered at once.
    let entity = "/monitoring/v4.1/serviceability/audits/8f14e45f-ceea-467a-9c33-000000000001";
    let r = tokio::time::timeout(Duration::from_secs(5), get(&pc, entity, "secret"))
        .await
        .expect("an entity GET under a held list path is not held");
    assert_eq!(r.status(), 200);

    gate.release(1);
    let r = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("released")
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["metadata"]["totalAvailableResults"], 3,
        "the v4.3 fixture, answered at v4.1"
    );
}

/// `mutate_after_list` counts answers on the list path it names, and the change lands *behind*
/// the answer that completes the countdown: that answer still carries the old value, and the
/// request after it sees the new one. The exactness is the whole point - it is how a test puts
/// an entity change between two requests a client makes back to back, with nothing to race.
#[tokio::test]
async fn mutate_after_list_lands_behind_the_answer_that_completes_its_countdown() {
    let pc = MockPc::builder().start().await;
    pc.mutate_after_list(VMS, WEB01, "name", "renamed-01", 2);

    let name = |v: &Value| {
        v["data"][0]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let first: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(name(&first), "web-01", "one answer short of the countdown");
    let second: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(name(&second), "web-01", "the answer that completes it");
    let third: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(name(&third), "renamed-01", "and the change is behind it");

    // One-shot: it fired, so the next answer is the same as the last.
    let fourth: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(name(&fourth), "renamed-01");

    // Another path's answers do not count against it: this one is armed on the VMs and the
    // audits are read three times without firing anything.
    pc.mutate_after_list(VMS, WEB01, "name", "renamed-again", 1);
    for _ in 0..3 {
        get(&pc, "/monitoring/v4.1/serviceability/audits", "secret").await;
    }
    let after: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(name(&after), "renamed-01", "the VMs' own answer fires it");
    let last: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(name(&last), "renamed-again");
}

/// A store the caller built is served as it is: nothing is read from disk, and the tasks path
/// falls back to the catalog's own version when the store has no prism fixtures.
#[tokio::test]
async fn the_builder_serves_an_in_memory_store() {
    let mut store = Store::default();
    store.insert(
        VMS,
        vec![
            json!({"extId": "a", "name": "vm-a", "powerState": "ON"}),
            json!({"extId": "b", "name": "vm-b", "powerState": "OFF"}),
        ],
    );
    let pc = MockPc::builder().store(store).start().await;
    let r = get(&pc, VMS, "secret").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["metadata"]["totalAvailableResults"], 2);
    assert_eq!(v["data"][1]["name"], "vm-b");
    // Not the bundled tree: a list only the fixtures have is empty here.
    let v: Value = get(&pc, "/clustermgmt/v4.3/config/clusters", "secret")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["metadata"]["totalAvailableResults"], 0);
    // A mutation still lands a task, and it walks.
    let r = act(
        &pc,
        "vmm.ahv.config.Vm",
        "power-off",
        &format!("{VMS}/a/$actions/power-off"),
        None,
    )
    .await;
    assert_eq!(r.status(), 202);
    let id = task_id(r).await;
    assert_eq!(task(&pc, &id).await["data"]["status"], "QUEUED");
}

/// `try_start` is `start` with the failure handed back: a fixture tree that does not parse is
/// an `Err` naming the file, not a panic. (A bind of `127.0.0.1:0` cannot be made to fail
/// deterministically, so the fixture error is the one that stands in for every early exit.)
#[tokio::test]
async fn try_start_reports_an_unreadable_fixture_tree() {
    let dir = std::env::temp_dir().join(format!("nutsh-mockpc-bad-{}", std::process::id()));
    let list = dir.join("vmm/v4.3/ahv/config");
    std::fs::create_dir_all(&list).unwrap();
    std::fs::write(list.join("vms.json"), "{").unwrap();
    let result = MockPc::builder().fixtures(dir.clone()).try_start().await;
    let _ = std::fs::remove_dir_all(&dir);
    let err = result
        .err()
        .expect("a fixture that does not parse is an error, not a panic");
    assert!(err.to_string().contains("vms.json"), "{err}");
}

/// A list GET with a `$filter`, sent the way the client sends one: as a query parameter that
/// reqwest percent-encodes and the mock decodes.
async fn filtered(pc: &MockPc, path: &str, filter: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(url(pc, path))
        .query(&[("$filter", filter)])
        .basic_auth("admin", Some("secret"))
        .send()
        .await
        .unwrap()
}

const ALERTS: &str = "/monitoring/v4.3/serviceability/alerts";
const TASKS: &str = "/prism/v4.4/config/tasks";
const LAB_CLUSTER: &str = "0006158a-2f0d-4d5a-8e2d-000000000010";

#[tokio::test]
async fn a_filter_narrows_the_list_when_honoured() {
    const OFF: &str = "powerState eq Vmm.Ahv.Config.PowerState'OFF'";
    let ignoring = MockPc::builder().start().await;
    let v: Value = filtered(&ignoring, VMS, OFF).await.json().await.unwrap();
    assert_eq!(
        v["metadata"]["totalAvailableResults"], 3,
        "the default mock ignores $filter, as every existing test assumes"
    );

    let pc = MockPc::builder().honours_filter().start().await;
    let v: Value = filtered(&pc, VMS, OFF).await.json().await.unwrap();
    assert_eq!(
        v["metadata"]["totalAvailableResults"], 1,
        "the total is the filtered count, which is what a header counter reads"
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"][0]["name"], "web-02");

    // Paging counts against the narrowed list: page 1 of a one-row list is empty.
    let r = reqwest::Client::new()
        .get(url(&pc, VMS))
        .query(&[("$filter", OFF), ("$page", "1"), ("$limit", "1")])
        .basic_auth("admin", Some("secret"))
        .send()
        .await
        .unwrap();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 0);
    assert_eq!(v["metadata"]["totalAvailableResults"], 1);
}

/// Every `$filter` the program sends, spelled as the source spells it, with the count the
/// bundled fixtures answer. A spelling added to `pages.toml`, `stats.rs`, `evidence.rs` or
/// `search.rs` without a row here is the alarm the demo would raise as a pane error.
#[tokio::test]
async fn every_filter_the_program_sends_parses() {
    let pc = MockPc::builder().honours_filter().start().await;
    let scoped = |f: &str, field: &str| format!("{f} and {field} eq '{LAB_CLUSTER}'");
    let table: Vec<(&str, String, u64)> = vec![
        // pages.toml:143, :474
        (ALERTS, "isResolved eq false".into(), 2),
        // pages.toml:158, stats.rs:117
        (TASKS, "status eq Prism.Config.TaskStatus'RUNNING'".into(), 1),
        // pages.toml:203
        (
            ALERTS,
            "isResolved eq false and (severity eq Monitoring.Common.Severity'CRITICAL' or severity eq Monitoring.Common.Severity'WARNING')".into(),
            2,
        ),
        // pages.toml:218
        (TASKS, "status eq Prism.Config.TaskStatus'FAILED'".into(), 1),
        // pages.toml:234
        (VMS, "powerState eq Vmm.Ahv.Config.PowerState'OFF'".into(), 1),
        // stats.rs:95
        (VMS, "powerState eq Vmm.Ahv.Config.PowerState'ON'".into(), 2),
        // stats.rs:101, :107
        (
            ALERTS,
            "severity eq Monitoring.Common.Severity'CRITICAL' and isResolved eq false".into(),
            1,
        ),
        (
            ALERTS,
            "severity eq Monitoring.Common.Severity'WARNING' and isResolved eq false".into(),
            1,
        ),
        // stats.rs:245-256: a pinned cluster scopes the counter on `cluster/extId` (VMs) or
        // `clusterUUID` (alerts).
        (
            VMS,
            scoped("powerState eq Vmm.Ahv.Config.PowerState'ON'", "cluster/extId"),
            2,
        ),
        (
            ALERTS,
            scoped(
                "severity eq Monitoring.Common.Severity'CRITICAL' and isResolved eq false",
                "clusterUUID",
            ),
            1,
        ),
        // evidence.rs:30-36: unquoted instants either side of a failed task's start. The
        // WARNING alert (09:50:46) is inside this window, the CRITICAL one (09:40:00) is not.
        (
            ALERTS,
            "creationTime ge 2026-09-05T09:49:00Z and creationTime le 2026-09-05T10:09:00Z".into(),
            1,
        ),
        // search.rs:183 and :238-240 (a quote doubled), :187
        (VMS, "startswith(name,'web')".into(), 2),
        (VMS, "startswith(name,'o''brien')".into(), 0),
        (VMS, format!("extId eq '{WEB01}'"), 1),
    ];
    for (path, filter, expected) in table {
        let r = filtered(&pc, path, &filter).await;
        assert_eq!(r.status(), 200, "{filter}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["metadata"]["totalAvailableResults"], expected, "{filter}");
    }
}

#[tokio::test]
async fn an_unparseable_filter_is_400() {
    let pc = MockPc::builder().honours_filter().start().await;
    for bad in [
        "status eq RUNNING",
        "powerState eq",
        "name contains 'web'",
        "(isResolved eq false",
        "isResolved eq false and",
    ] {
        let r = filtered(&pc, VMS, bad).await;
        assert_eq!(r.status(), 400, "{bad}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["data"]["$objectType"], "prism.v4.error.ErrorResponse");
        let message = v["data"]["error"][0]["message"].as_str().unwrap();
        assert!(message.starts_with("$filter: "), "{bad}: {message}");
    }
    // The same string is fine on the mock that ignores the parameter.
    let ignoring = MockPc::builder().start().await;
    assert_eq!(
        filtered(&ignoring, VMS, "status eq RUNNING").await.status(),
        200
    );
}

/// With `live_clock` a task is stamped with the wall clock instead of the constants: created
/// between two readings taken around the request, and every later step no earlier than the
/// one before. Whole seconds and `Z` on both sides, so the strings order as the instants do.
#[tokio::test]
async fn a_live_clock_task_is_stamped_now() {
    let fixed = MockPc::builder().start().await;
    let id = task_id(
        act(
            &fixed,
            "vmm.ahv.config.Vm",
            "power-off",
            &format!("{VMS}/{WEB01}/$actions/power-off"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        task(&fixed, &id).await["data"]["createdTime"],
        "2026-09-05T09:59:00Z",
        "the default is the constant every snapshot was drawn with"
    );

    let pc = MockPc::builder().live_clock().start().await;
    let before = nutsh_mockpc::rfc3339(std::time::SystemTime::now());
    let id = task_id(
        act(
            &pc,
            "vmm.ahv.config.Vm",
            "power-off",
            &format!("{VMS}/{WEB01}/$actions/power-off"),
            None,
        )
        .await,
    )
    .await;
    let after = nutsh_mockpc::rfc3339(std::time::SystemTime::now());
    let queued = task(&pc, &id).await;
    let created = queued["data"]["createdTime"].as_str().unwrap().to_string();
    assert!(
        before.as_str() <= created.as_str() && created.as_str() <= after.as_str(),
        "{before} <= {created} <= {after}"
    );
    assert_eq!(created.len(), 20, "whole seconds, Z: {created}");
    let running = task(&pc, &id).await;
    assert!(running["data"]["startedTime"].as_str().unwrap() >= created.as_str());
    task(&pc, &id).await;
    let done = task(&pc, &id).await;
    assert_eq!(done["data"]["status"], "SUCCEEDED");
    assert!(done["data"]["completedTime"].as_str().unwrap() >= created.as_str());
    assert_ne!(done["data"]["completedTime"], "2026-09-05T10:00:00Z");
}

/// Walk a mock-created task to its terminal state and return it: the default walk is four
/// steps, so five reads is one more than enough, and a task that never gets there is the
/// failure.
async fn finish(pc: &MockPc, id: &str) -> Value {
    for _ in 0..5 {
        let t = task(pc, id).await;
        if matches!(
            t["data"]["status"].as_str(),
            Some("SUCCEEDED" | "FAILED" | "CANCELED")
        ) {
            return t;
        }
    }
    panic!("task {id} never reached a terminal state");
}

const ALERT01: &str = "9ead870c-d76f-4475-8a13-000000000001";

/// Three curated actions share the `manage-alert` endpoint, told apart by the constant body
/// each sends: the task is named by the body that arrived, not by whichever action the catalog
/// lists first.
#[tokio::test]
async fn a_shared_endpoint_names_the_task_by_its_body() {
    let pc = MockPc::builder().start().await;
    let action = format!("{ALERTS}/{ALERT01}/$actions/manage-alert");
    let ack = task_id(
        act(
            &pc,
            "monitoring.serviceability.Alert",
            "acknowledge",
            &action,
            Some(json!({"actionType": "ACKNOWLEDGE"})),
        )
        .await,
    )
    .await;
    assert_eq!(task(&pc, &ack).await["data"]["operation"], "acknowledge");
    let resolve = task_id(
        act(
            &pc,
            "monitoring.serviceability.Alert",
            "resolve",
            &action,
            Some(json!({"actionType": "RESOLVE"})),
        )
        .await,
    )
    .await;
    let t = task(&pc, &resolve).await;
    assert_eq!(t["data"]["operation"], "resolve");
    assert_eq!(
        t["data"]["operationDescription"],
        "resolve on Disk space usage high for /home on Controller VM 203.0.113.11"
    );
}

#[tokio::test]
async fn resolve_marks_the_alert_resolved() {
    let pc = MockPc::builder().honours_filter().start().await;
    let alert = format!("{ALERTS}/{ALERT01}");
    let action = format!("{alert}/$actions/manage-alert");
    let ack = act(
        &pc,
        "monitoring.serviceability.Alert",
        "acknowledge",
        &action,
        Some(json!({"actionType": "ACKNOWLEDGE"})),
    )
    .await;
    assert_eq!(ack.status(), 202);
    finish(&pc, &task_id(ack).await).await;
    let a: Value = get(&pc, &alert, "secret").await.json().await.unwrap();
    assert_eq!(a["data"]["isAcknowledged"], true);
    assert_eq!(a["data"]["status"], "ACKNOWLEDGED");
    assert_eq!(a["data"]["acknowledgedTime"], "2026-09-05T10:00:00Z");
    assert_eq!(a["data"]["acknowledgedByUsername"], "admin");
    assert_eq!(
        a["data"]["isResolved"], false,
        "acknowledged is not resolved"
    );

    let resolve = act(
        &pc,
        "monitoring.serviceability.Alert",
        "resolve",
        &action,
        Some(json!({"actionType": "RESOLVE"})),
    )
    .await;
    assert_eq!(resolve.status(), 202);
    let id = task_id(resolve).await;
    // Nothing moves before the terminal step.
    for _ in 0..3 {
        assert_ne!(task(&pc, &id).await["data"]["status"], "SUCCEEDED");
        let a: Value = get(&pc, &alert, "secret").await.json().await.unwrap();
        assert_eq!(a["data"]["isResolved"], false);
    }
    assert_eq!(task(&pc, &id).await["data"]["status"], "SUCCEEDED");
    let a: Value = get(&pc, &alert, "secret").await.json().await.unwrap();
    assert_eq!(a["data"]["isResolved"], true);
    assert_eq!(a["data"]["status"], "RESOLVED");
    assert_eq!(a["data"]["resolvedTime"], "2026-09-05T10:00:00Z");
    assert_eq!(a["data"]["resolvedByUsername"], "admin");
    // And the Dashboard's filter no longer counts it.
    let v: Value = filtered(&pc, ALERTS, "isResolved eq false")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["metadata"]["totalAvailableResults"], 1);
}

#[tokio::test]
async fn enter_maintenance_moves_the_host() {
    let pc = MockPc::builder().start().await;
    let host = "7b2f2f70-0f6a-4b58-9f9b-000000000020";
    let scoped = format!("/clustermgmt/v4.3/config/clusters/{LAB_CLUSTER}/hosts/{host}");
    let top = format!("/clustermgmt/v4.3/config/hosts/{host}");
    let operations = format!("/clustermgmt/v4.3/operations/clusters/{LAB_CLUSTER}/hosts/{host}");
    let state = |v: Value| v["data"]["maintenanceState"].as_str().unwrap().to_string();

    let r = act(
        &pc,
        "clustermgmt.config.Host~hosts",
        "enter-host-maintenance",
        &format!("{operations}/$actions/enter-host-maintenance"),
        Some(json!({"shouldRollbackOnFailure": true})),
    )
    .await;
    assert_eq!(r.status(), 202);
    finish(&pc, &task_id(r).await).await;
    let in_scoped: Value = get(&pc, &scoped, "secret").await.json().await.unwrap();
    assert_eq!(state(in_scoped), "IN_MAINTENANCE");
    let in_top: Value = get(&pc, &top, "secret").await.json().await.unwrap();
    assert_eq!(
        state(in_top),
        "IN_MAINTENANCE",
        "the Hosts page reads the top-level list"
    );

    let r = act(
        &pc,
        "clustermgmt.config.Host~hosts",
        "exit-host-maintenance",
        &format!("{operations}/$actions/exit-host-maintenance"),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status(), 202);
    finish(&pc, &task_id(r).await).await;
    let out: Value = get(&pc, &top, "secret").await.json().await.unwrap();
    assert_eq!(state(out), "NORMAL");
}

#[tokio::test]
async fn migrate_to_host_moves_the_vm() {
    let pc = MockPc::builder().start().await;
    // The mock does not check that the host exists: it is not a scheduler, and the picker
    // that fills this body only offers hosts the list has.
    let target = "7b2f2f70-0f6a-4b58-9f9b-000000000021";
    let r = act(
        &pc,
        "vmm.ahv.config.Vm",
        "migrate-to-host",
        &format!("{VMS}/{WEB01}/$actions/migrate-to-host"),
        Some(json!({"host": {"extId": target}})),
    )
    .await;
    assert_eq!(r.status(), 202);
    finish(&pc, &task_id(r).await).await;
    let vm: Value = get(&pc, &format!("{VMS}/{WEB01}"), "secret")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(vm["data"]["host"]["extId"], target);
    assert_eq!(
        vm["data"]["powerState"], "ON",
        "a migration is not a power action"
    );
}

#[tokio::test]
async fn clone_adds_a_vm() {
    let pc = MockPc::builder().start().await;
    let r = act(
        &pc,
        "vmm.ahv.config.Vm",
        "clone",
        &format!("{VMS}/{WEB01}/$actions/clone"),
        Some(json!({"name": "web-03"})),
    )
    .await;
    assert_eq!(r.status(), 202);
    let done = finish(&pc, &task_id(r).await).await;
    assert_eq!(done["data"]["entitiesAffected"][0]["name"], "web-01");
    let list: Value = get(&pc, VMS, "secret").await.json().await.unwrap();
    assert_eq!(list["metadata"]["totalAvailableResults"], 4);
    let rows = list["data"].as_array().unwrap();
    let clone = rows
        .iter()
        .find(|r| r["name"] == "web-03")
        .expect("the clone");
    assert_ne!(clone["extId"], WEB01);
    assert_eq!(clone["powerState"], "OFF", "a clone is created off");
    assert!(clone.get("host").is_none(), "an OFF VM has no host");
    assert_eq!(
        clone["disks"].as_array().unwrap().len(),
        rows[0]["disks"].as_array().unwrap().len(),
        "the clone carries the original's disks"
    );
    assert_eq!(clone["updateTime"], "2026-09-05T10:00:00Z");
    // The original is untouched.
    let original: Value = get(&pc, &format!("{VMS}/{WEB01}"), "secret")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(original["data"]["name"], "web-01");
    assert_eq!(original["data"]["powerState"], "ON");
}

#[tokio::test]
async fn a_recovery_point_lands_in_the_list() {
    let pc = MockPc::builder().start().await;
    const RPS: &str = "/dataprotection/v4.4/config/recovery-points";
    let before: Value = get(&pc, RPS, "secret").await.json().await.unwrap();
    assert_eq!(before["metadata"]["totalAvailableResults"], 5);
    // The VM's `snapshot` action posts to the recovery-points list (generated.rs:9571) with
    // the body its form builds: name, the VM under vmRecoveryPoints[0], an expiry.
    let r = act(
        &pc,
        "vmm.ahv.config.Vm",
        "snapshot",
        RPS,
        Some(json!({
            "name": "web-01-before-upgrade",
            "vmRecoveryPoints": [{"vmExtId": WEB01}],
            "expirationTime": "2026-10-05T10:00:00Z"
        })),
    )
    .await;
    assert_eq!(r.status(), 202);
    let done = finish(&pc, &task_id(r).await).await;
    assert_eq!(
        done["data"]["numberOfEntitiesAffected"], 0,
        "a list-level create"
    );
    let after: Value = get(&pc, RPS, "secret").await.json().await.unwrap();
    assert_eq!(after["metadata"]["totalAvailableResults"], 6);
    let rp = after["data"].as_array().unwrap().last().unwrap();
    assert_eq!(rp["name"], "web-01-before-upgrade");
    assert_eq!(rp["status"], "COMPLETE");
    assert_eq!(rp["recoveryPointType"], "CRASH_CONSISTENT");
    assert_eq!(rp["creationTime"], "2026-09-05T10:00:00Z");
    assert_eq!(rp["expirationTime"], "2026-10-05T10:00:00Z");
    assert_eq!(rp["vmRecoveryPoints"][0]["vmExtId"], WEB01);
    assert!(rp["extId"].as_str().is_some_and(|id| !id.is_empty()));
    // And it is an entity: a GET by its extId answers.
    let id = rp["extId"].as_str().unwrap();
    assert_eq!(
        get(&pc, &format!("{RPS}/{id}"), "secret").await.status(),
        200
    );
}
