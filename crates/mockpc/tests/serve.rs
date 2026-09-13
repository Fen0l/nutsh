use std::time::Duration;

use nutsh_mockpc::{Gate, MockPc};
use serde_json::Value;

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
