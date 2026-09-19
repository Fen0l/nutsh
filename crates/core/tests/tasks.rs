mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use nutsh_catalog::{Field, FieldType, Kind, kind};
use nutsh_core::actions::{self, Outcome};
use nutsh_core::scheduler::{Msg, Scheduler, Subscription};
use nutsh_core::store::TableKey;
use nutsh_core::tasks::{TaskIndex, TaskStatus};
use nutsh_mockpc::MockPc;
use nutsh_prism::Entity;
use serde_json::json;
use tokio::sync::mpsc;

const WEB01: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";

fn vm() -> &'static Kind {
    kind("vmm.ahv.config.Vm").unwrap()
}

fn entity(raw: serde_json::Value) -> Entity {
    Entity::new(vm(), raw, None)
}

#[test]
fn build_body_writes_nested_paths_and_substitutes() {
    let now = UNIX_EPOCH + Duration::from_secs(1_788_602_400); // 2026-09-05T10:00:00Z
    let fields = &[
        Field {
            name: "name",
            label: "Name",
            ty: FieldType::Text,
            required: true,
            value: None,
            hidden: false,
        },
        Field {
            name: "vmRecoveryPoints[0].vmExtId",
            label: "",
            ty: FieldType::Text,
            required: false,
            value: Some("$ext_id"),
            hidden: true,
        },
        Field {
            name: "expirationTime",
            label: "Expires",
            ty: FieldType::Text,
            required: true,
            value: Some("$now+30d"),
            hidden: false,
        },
        Field {
            name: "spec.count",
            label: "Count",
            ty: FieldType::Integer,
            required: false,
            value: Some("3"),
            hidden: false,
        },
        Field {
            name: "spec.on",
            label: "On",
            ty: FieldType::Bool,
            required: false,
            value: Some("true"),
            hidden: false,
        },
    ];
    let mut entered = HashMap::new();
    entered.insert("name", "rp-1".to_string());
    let e = entity(json!({"extId": WEB01, "name": "web-01"}));
    let body = actions::build_body(fields, &entered, &e, now).unwrap();
    assert_eq!(body["name"], "rp-1");
    assert_eq!(body["vmRecoveryPoints"][0]["vmExtId"], WEB01);
    assert_eq!(body["expirationTime"], "2026-10-05T10:00:00Z");
    assert_eq!(body["spec"]["count"], json!(3), "an integer, not a string");
    assert_eq!(body["spec"]["on"], json!(true));

    // A missing required field and an unknown $name are both errors, so an overlay typo fails
    // on first use rather than being sent as a literal.
    let err = actions::build_body(fields, &HashMap::new(), &e, now).unwrap_err();
    // By the label the form shows, not the body path it writes.
    assert!(err.contains("Name"), "{err}");
    let bad = &[Field {
        name: "x",
        label: "",
        ty: FieldType::Text,
        required: false,
        value: Some("$nope"),
        hidden: true,
    }];
    let err = actions::build_body(bad, &HashMap::new(), &e, now).unwrap_err();
    assert!(err.contains("$nope"), "{err}");
}

/// An update form starts from what is there. The values come back as the strings the form
/// edits, and a field the user does not touch has to round-trip through `build_body` as the
/// value it came from: a form that starts blank invites clearing a field nobody meant to touch.
#[test]
fn current_values_reads_what_the_entity_already_has() {
    let now = UNIX_EPOCH + Duration::from_secs(1_788_602_400);
    let field = |name: &'static str, ty: FieldType| Field {
        name,
        label: "",
        ty,
        required: false,
        value: None,
        hidden: false,
    };
    let fields = &[
        field("name", FieldType::Text),
        field("memorySizeBytes", FieldType::Integer),
        field("isCpuPassthroughEnabled", FieldType::Bool),
        field("categories", FieldType::Json("[{}]")),
        field("description", FieldType::Text),
        field("nics[0].backingInfo.macAddress", FieldType::Text),
        field("cluster.extId", FieldType::Reference(None)),
    ];
    let e = entity(json!({
        "extId": WEB01,
        "name": "web-01",
        "memorySizeBytes": 8_589_934_592u64,
        "isCpuPassthroughEnabled": false,
        "categories": [{"extId": "c-1"}],
        "description": serde_json::Value::Null,
        "nics": [{"backingInfo": {"macAddress": "50:6b:8d:00:00:01"}}],
        "cluster": {"extId": "cl-1"},
    }));
    let current = actions::current_values(fields, &e);
    assert_eq!(current.get("name").map(String::as_str), Some("web-01"));
    assert_eq!(
        current.get("memorySizeBytes").map(String::as_str),
        Some("8589934592"),
        "a number is the digits, not a quoted string"
    );
    assert_eq!(
        current.get("isCpuPassthroughEnabled").map(String::as_str),
        Some("false"),
        "and false is a value, not an absence"
    );
    assert_eq!(
        current.get("categories").map(String::as_str),
        Some(r#"[{"extId":"c-1"}]"#)
    );
    assert_eq!(
        current
            .get("nics[0].backingInfo.macAddress")
            .map(String::as_str),
        Some("50:6b:8d:00:00:01"),
        "the read half of the same path grammar `write_at` writes"
    );
    assert_eq!(
        current.get("cluster.extId").map(String::as_str),
        Some("cl-1")
    );
    // Null is nothing, and so is a path the document does not have: an empty field is honest
    // where an invented one is not.
    assert_eq!(current.get("description"), None);
    assert_eq!(current.get("nowhere.at.all"), None);

    // And what came out goes back in unchanged.
    let entered: HashMap<&str, String> = current.iter().map(|(k, v)| (*k, v.clone())).collect();
    let body = actions::build_body(fields, &entered, &e, now).unwrap();
    assert_eq!(body["name"], "web-01");
    assert_eq!(body["memorySizeBytes"], json!(8_589_934_592u64));
    assert_eq!(body["isCpuPassthroughEnabled"], json!(false));
    assert_eq!(body["categories"], json!([{"extId": "c-1"}]));
    assert_eq!(
        body["nics"][0]["backingInfo"]["macAddress"],
        "50:6b:8d:00:00:01"
    );
    assert_eq!(body["cluster"]["extId"], "cl-1");
    assert!(body.get("description").is_none(), "{body}");
}

/// The other way the generator writes a reference: `cluster` addressing the object, which is
/// how every one of the twenty-six in the shipped catalog is written. `ClusterReference` is
/// `type: object, required: [extId], additionalProperties: false`, so the string that
/// `FieldType::Reference` would otherwise fall through to is a wrong body - and on an update
/// it would go out over the object the Prism Central already had.
///
/// Two shapes reach `build_body` and both have to leave as the object: what a prefill read off
/// the entity, which is the reference's own wire form, and the bare ext id a picker leaves.
#[test]
fn a_reference_goes_out_as_the_object_its_schema_says_it_is() {
    let now = UNIX_EPOCH + Duration::from_secs(1_788_602_400);
    let field = |name: &'static str| Field {
        name,
        label: "",
        ty: FieldType::Reference(None),
        required: false,
        value: None,
        hidden: false,
    };
    let fields = &[field("cluster"), field("subnet"), field("rack.extId")];
    let e = entity(json!({
        "extId": WEB01,
        "cluster": {"extId": "cl-1"},
        "rack": {"extId": "rk-1"},
    }));

    // The prefill half: what `current_values` read back goes out unchanged.
    let current = actions::current_values(fields, &e);
    assert_eq!(
        current.get("cluster").map(String::as_str),
        Some(r#"{"extId":"cl-1"}"#),
        "the reference's own wire form, because a field is one line"
    );
    let entered: HashMap<&str, String> = current.iter().map(|(k, v)| (*k, v.clone())).collect();
    let body = actions::build_body(fields, &entered, &e, now).unwrap();
    assert_eq!(body["cluster"], json!({"extId": "cl-1"}));
    assert_ne!(
        body["cluster"],
        json!(r#"{"extId":"cl-1"}"#),
        "and never the wire form quoted into a string"
    );
    // A path that already addresses the id inside the reference stays the string it was.
    assert_eq!(body["rack"]["extId"], "rk-1");

    // The picker half: a bare ext id is wrapped rather than sent flat.
    let picked: HashMap<&str, String> = HashMap::from([("subnet", "sn-1".to_string())]);
    let body = actions::build_body(fields, &picked, &e, now).unwrap();
    assert_eq!(body["subnet"], json!({"extId": "sn-1"}));
}

#[test]
fn plan_resolves_action_kind_and_action_parents() {
    let host = kind("clustermgmt.config.Host").unwrap();
    let target = host.action_target();
    let action = target.action("enter-host-maintenance").unwrap();
    let row = Entity::new(
        host,
        json!({"extId": "h1", "hostName": "host-3", "cluster": {"uuid": "c1"}}),
        None,
    );
    let p = actions::plan(host, &row, action, &[], Some(json!({}))).unwrap();
    assert_eq!(p.kind.id, "clustermgmt.config.Host~hosts");
    assert_eq!(p.parents, vec!["c1".to_string()]);
    assert_eq!(p.ext_id, "h1");
    assert_eq!(p.name, "host-3");

    // A row that cannot supply the parent is an error, not a request with a hole in it.
    let bare = Entity::new(host, json!({"extId": "h1", "hostName": "host-3"}), None);
    let err = actions::plan(host, &bare, action, &[], Some(json!({}))).unwrap_err();
    assert!(err.contains("parent"), "{err}");
}

#[tokio::test]
async fn execute_starts_a_task_the_watch_follows_to_success() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let row = entity(json!({"extId": WEB01, "name": "web-01"}));
    let plan = actions::plan(vm(), &row, vm().action("power-off").unwrap(), &[], None).unwrap();
    let task = match actions::execute(&client, &plan).await {
        Outcome::Started(t) => t,
        other => panic!("{other:?}"),
    };

    let mut index = TaskIndex::default();
    let journal = nutsh_core::journal::JournalId::default();
    index.watch(&mut scheduler, None, &plan, task.clone(), journal);
    let mut finished = None;
    let mut seen = Vec::new();
    while finished.is_none() {
        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a message")
            .expect("open");
        if let Msg::Entity { entity, .. } = &msg {
            seen.push(index.status_of(&task.ext_id));
            finished = index.apply(entity);
        }
    }
    let done = finished.unwrap();
    assert_eq!(done.watch.status, TaskStatus::Succeeded);
    assert_eq!(done.watch.progress, 100);
    assert!(seen.contains(&Some(TaskStatus::Queued)));
    assert!(seen.contains(&Some(TaskStatus::Running)));
    assert!(
        index.running().next().is_none(),
        "a terminal watch is dropped"
    );
    // The affected entity is what the caller refreshes, and it is not the source for a create.
    assert_eq!(done.watch.ext_id, WEB01);
    scheduler.unsubscribe(done.watch.sub);
}

#[tokio::test]
async fn a_once_subscription_runs_one_cycle_and_the_receiver_reaps_it() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let key = TableKey::top(vm());
    let id = scheduler.subscribe(Subscription::once(key.clone(), WEB01.to_string()));
    // One cycle is one `get_in` plus its `Entity`, then the cycle's own `Complete`, then the
    // `Done` that says the task has returned.
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(matches!(got[0], Msg::Entity { .. }));
    assert!(matches!(got[1], Msg::Complete { .. }));
    let Msg::Done { epoch, sub } = got[2] else {
        panic!("a `Done` last, got {:?}", got[2])
    };
    assert_eq!(sub, id);
    // Still live once everything it sent has been drained: nothing it queued can be gated out
    // by `is_live`, which is the entire reason `once` exists.
    assert!(scheduler.is_live(id));
    // And opening another subscription mid-drain does not reap it - the receiver does, when it
    // drains the `Done` above. A sweep here would discard a queued refresh.
    let other = scheduler.subscribe(Subscription::once(key, WEB01.to_string()));
    assert!(
        scheduler.is_live(id),
        "not reaped by an unrelated subscribe"
    );
    // A `Done` from a previous scheduler names an id this one has since reissued; the epoch is
    // what stops it silently unsubscribing a live view after a context switch.
    scheduler.reap(epoch + 1, other);
    assert!(scheduler.is_live(other), "another scheduler's `Done`");
    scheduler.reap(epoch, id);
    assert!(!scheduler.is_live(id), "reaped by its own `Done`");
    assert!(scheduler.is_live(other));
    // And exactly one cycle: past `MIN_INTERVAL`, a second one from `id` would have run by
    // now, so draining everything queued and finding nothing of `id`'s is what proves the loop
    // stopped.
    tokio::time::sleep(Duration::from_millis(1_400)).await;
    let mut drained = 0;
    while let Ok(msg) = rx.try_recv() {
        assert_eq!(msg.sub(), Some(other), "only the second subscription's");
        drained += 1;
    }
    assert_eq!(drained, 3, "the second subscription's own one cycle");
}

#[tokio::test]
async fn a_failed_and_a_stalled_task_are_reported_honestly() {
    let pc = MockPc::builder().fail_task("power-on").start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let row = entity(json!({"extId": WEB01, "name": "web-01"}));
    let plan = actions::plan(vm(), &row, vm().action("power-on").unwrap(), &[], None).unwrap();
    let Outcome::Started(task) = actions::execute(&client, &plan).await else {
        panic!("expected a task");
    };
    let mut index = TaskIndex::default();
    index.watch(
        &mut scheduler,
        None,
        &plan,
        task,
        nutsh_core::journal::JournalId::default(),
    );
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Msg::Entity { entity, .. } = &msg
            && let Some(done) = index.apply(entity)
        {
            assert_eq!(done.watch.status, TaskStatus::Failed);
            assert!(!done.watch.errors.is_empty());
            scheduler.unsubscribe(done.watch.sub);
            break;
        }
    }
}

#[test]
fn a_watch_still_running_after_fifteen_minutes_gives_up_without_calling_it_a_failure() {
    let mut index = TaskIndex::default();
    let started = Instant::now();
    index.insert_for_test("ZXJnb24=:x", started);
    assert!(
        index
            .expire(started + Duration::from_secs(14 * 60))
            .is_empty()
    );
    let gone = index.expire(started + Duration::from_secs(16 * 60));
    assert_eq!(gone.len(), 1);
    assert!(
        gone[0].outcome_text().contains("still running after 15m"),
        "{:?}",
        gone[0].outcome_text()
    );
}

/// Whether a `Subscription::watching()` must be exempt from `$select`.
///
/// It needs no exemption, because no projection can reach it. `watching()` is a modifier on
/// [`Subscription::single`]; a `single` cycle is `fetch_one`, which calls `Client::get_in`;
/// and `get_in` issues `self.http.get(url)` with **no query at all** - there is no
/// `ListOptions` on that path and nothing to hang a `$select` on. `$select` is built in
/// `Client::list_page_at`, which a watch never enters.
///
/// This pins both halves: the watch's requests carry no query, and the entity that reaches
/// `TaskIndex::apply` - the one the progress line is read off - still carries
/// `completionDetails`, which no Tasks *column* reads and which a column-derived `$select`
/// would therefore have dropped.
#[tokio::test]
async fn a_task_watch_carries_no_query_at_all_so_no_select_can_narrow_it() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let row = entity(json!({"extId": WEB01, "name": "web-01"}));
    let plan = actions::plan(vm(), &row, vm().action("power-off").unwrap(), &[], None).unwrap();
    let task = match actions::execute(&client, &plan).await {
        Outcome::Started(t) => t,
        other => panic!("{other:?}"),
    };

    let mut index = TaskIndex::default();
    index.watch(
        &mut scheduler,
        None,
        &plan,
        task.clone(),
        nutsh_core::journal::JournalId::default(),
    );
    let mut finished = None;
    let mut whole = 0usize;
    while finished.is_none() {
        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a message")
            .expect("open");
        if let Msg::Entity { entity, .. } = &msg {
            // `operation` is the field `core::detail`'s "What" section renders and no Tasks
            // *column* reads - the very one a column-derived `$select` blanks, as
            // `crates/core/tests/detail.rs` asserts. On a watch it is there every time.
            assert!(
                !kind("prism.config.Task")
                    .expect("Tasks")
                    .columns
                    .iter()
                    .any(|c| c.path.starts_with("operation")
                        && !c.path.starts_with("operationDescription")),
                "`operation` became a column; pick another field for this assertion"
            );
            if entity.raw.get("operation").is_some() {
                whole += 1;
            }
            finished = index.apply(entity);
        }
    }
    let done = finished.expect("the watch finished");
    assert_eq!(done.watch.status, TaskStatus::Succeeded);
    assert!(
        whole > 0,
        "the watch never saw a whole task; the exemption claim is untested"
    );

    // The point of the test: every request the watch made was a bare GET.
    let watched: Vec<_> = pc
        .requests()
        .into_iter()
        .filter(|r| r.path.contains("/config/tasks/"))
        .collect();
    assert!(!watched.is_empty(), "the watch made no request");
    for r in &watched {
        assert_eq!(r.method, "GET");
        assert!(
            r.query.is_empty(),
            "a watch's GET carried a query, so a $select could reach it: {:?}",
            r.query
        );
    }
    scheduler.unsubscribe(done.watch.sub);
}
