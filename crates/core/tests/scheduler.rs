mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use nutsh_catalog::{DEFAULT_MAX_ROWS, KINDS, kind};
use nutsh_core::scheduler::{Msg, Scheduler, SubId, Subscription};
use nutsh_core::store::{Failure, Store, TableKey, Update};
use nutsh_mockpc::{Gate, MockPc};
use nutsh_prism::{Availability, Client, NamespaceStatus, Profile};
use tokio::sync::mpsc;

const WEB01: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
const VMS: &str = "/vmm/v4.3/ahv/config/vms";

fn vms() -> TableKey {
    TableKey::top(kind("vmm.ahv.config.Vm").unwrap())
}

/// Drains until a cycle ends - so one of its pages is not mistaken for its end - and says
/// whose it was, which is the only way to tell one subscription's cycles from another's.
async fn next_complete(rx: &mut mpsc::Receiver<Msg>) -> SubId {
    loop {
        match rx.recv().await {
            Some(Msg::Complete { sub, .. }) => return sub,
            Some(_) => {}
            None => panic!("the poll channel closed"),
        }
    }
}

/// Drains messages into the store until the first `Complete` or `Error` for `key`.
async fn until_settled(rx: &mut mpsc::Receiver<Msg>, store: &mut Store, key: &TableKey) -> Msg {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("message")
            .expect("open");
        let settled =
            matches!(&msg, Msg::Complete { key: k, .. } | Msg::Error { key: k, .. } if k == key);
        // Every message a subscription sends is about a table; only the DR sampler's is not.
        let (k, update) = msg.clone().into_update().expect("a table message");
        store.apply(&k, update);
        if settled {
            return msg;
        }
    }
}

#[tokio::test]
async fn a_subscription_pages_then_completes_and_repeats() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    let sub = scheduler.subscribe(Subscription {
        page_size: 2,
        ..Subscription::list(key.clone(), Duration::from_millis(200))
    });

    let first = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(first, Msg::Complete { generation: 1, .. }));
    assert_eq!(store.table(&key).rows.len(), 3);
    assert_eq!(store.table(&key).total, Some(3));
    // Two pages of two, so two list requests in cycle one.
    assert_eq!(pc.requests_to(VMS).len(), 2);

    // The interval floors to a second, so the second cycle lands then, not after 200 ms.
    let second = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(second, Msg::Complete { generation: 2, .. }),
        "{second:?}"
    );
    scheduler.unsubscribe(sub);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = pc.requests_to(VMS).len();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        pc.requests_to(VMS).len(),
        after,
        "no requests after unsubscribe"
    );

    // Subscribing again continues the generation sequence, so the store accepts the new
    // cycle instead of dropping it as older than what it holds.
    let again = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(3600)));
    let third = until_settled(&mut rx, &mut store, &key).await;
    match third {
        Msg::Complete { generation, .. } => assert!(generation >= 3, "{generation}"),
        other => panic!("{other:?}"),
    }
    assert!(store.table(&key).generation >= 3);
    assert_eq!(store.table(&key).rows.len(), 3);
    scheduler.unsubscribe(again);
}

#[tokio::test]
async fn refresh_runs_a_cycle_now() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    let sub = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(3600)));
    until_settled(&mut rx, &mut store, &key).await;
    scheduler.refresh(sub);
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(msg, Msg::Complete { generation: 2, .. }),
        "{msg:?}"
    );
}

/// The cycle says it started before it asks for anything, so the frame can say `listing…` while
/// the first page is still in flight. Only a listing cycle: a `get_in` has one request and
/// nothing to walk.
#[tokio::test]
async fn a_listing_cycle_announces_itself_before_its_first_request() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let key = vms();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(60)));

    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("message")
        .expect("open");
    assert!(
        matches!(&first, Msg::Started { key: k, generation: 1, .. } if *k == key),
        "{first:?}"
    );
    // And it routes to the store like any other table message: `into_update` turns it into
    // `Update::Started` with no special case.
    assert!(
        matches!(
            first.into_update(),
            Some((k, Update::Started { generation: 1 })) if k == key
        ),
        "a Started routes to its own table as Update::Started"
    );

    // A single-entity subscription announces nothing.
    let (tx2, mut rx2) = mpsc::channel(64);
    let client2 = Arc::new(common::client(&pc).await);
    let mut single = Scheduler::new(client2, tx2);
    single.subscribe(Subscription::single(
        key.clone(),
        Duration::from_secs(60),
        WEB01.to_string(),
    ));
    let first = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
        .await
        .expect("message")
        .expect("open");
    assert!(matches!(first, Msg::Entity { .. }), "{first:?}");
}

#[tokio::test]
async fn an_error_is_reported_and_the_next_cycle_recovers() {
    // Two 429s in a row: the client's single retry sees the second, so cycle one fails.
    let pc = MockPc::builder()
        .rate_limit_once(VMS)
        .rate_limit_once(VMS)
        .start()
        .await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(100)));
    let first = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(first, Msg::Error { generation: 1, .. }),
        "{first:?}"
    );
    assert_eq!(
        store
            .table(&key)
            .error
            .as_ref()
            .map(Failure::text)
            .as_deref(),
        Some("rate limited (HTTP 429) - retrying")
    );
    let second = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(second, Msg::Complete { generation: 2, .. }),
        "{second:?}"
    );
    assert_eq!(store.table(&key).rows.len(), 3);
}

#[tokio::test]
async fn single_subscriptions_fetch_one_entity_and_nested_keys_fill_the_path() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    scheduler.subscribe(Subscription::single(
        key.clone(),
        Duration::from_secs(3600),
        WEB01.into(),
    ));
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Complete { .. }));
    assert_eq!(store.table(&key).rows.len(), 1);
    assert_eq!(store.table(&key).rows[WEB01].name, "web-01");

    let disks = TableKey::under(kind("vmm.ahv.config.Disk").unwrap(), vec![WEB01.into()]);
    scheduler.subscribe(Subscription::list(disks.clone(), Duration::from_secs(3600)));
    let msg = until_settled(&mut rx, &mut store, &disks).await;
    assert!(matches!(msg, Msg::Complete { .. }));
    assert_eq!(store.table(&disks).rows.len(), 2);
    assert!(!pc.requests_to(&format!("{VMS}/{WEB01}/disks")).is_empty());
}

#[tokio::test]
async fn a_non_paging_kind_lists_once() {
    // A kind whose list endpoint takes neither `$page` nor `$limit`: one request settles it,
    // whatever the page size the subscription asks for.
    let kind = KINDS
        .iter()
        .find(|k| {
            k.is_top_level()
                && !k.list_path.contains('{')
                && !k.list_params.page
                && !k.list_params.limit
                && !k.list_params.required
        })
        .expect("a non-paging kind in the catalog");
    let version = nutsh_catalog::version_in(kind.list_path).expect("a version in the list path");
    let (_, tail) = kind
        .list_path
        .split_once(&format!("/{version}"))
        .expect("the version splits the path");

    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = TableKey::top(kind);
    // Negotiation probes the shortest paths of every namespace and may have asked for this
    // one already, so the cycle's requests are counted from here.
    let before = pc.requests_to(tail).len();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(3600)));
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Complete { .. }), "{msg:?}");
    assert_eq!(
        pc.requests_to(tail).len(),
        before + 1,
        "{} pages once",
        kind.id
    );
}

#[tokio::test]
async fn a_closed_receiver_ends_the_task() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(100)));
    until_settled(&mut rx, &mut store, &key).await;
    drop(rx);
    // The interval floors to a second, so the cycle that finds the channel closed runs inside
    // this first wait; the task returns from it and never fetches again.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let after = pc.requests_to(VMS).len();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        pc.requests_to(VMS).len(),
        after,
        "no requests once the receiver is gone"
    );
}

/// A pane's query reaches the request; the client drops what the kind does not accept.
#[tokio::test]
async fn a_pane_subscription_sends_its_filter() {
    const RUNNING: &str = "status eq Prism.Config.TaskStatus'RUNNING'";
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let kind = kind("prism.config.Task").unwrap();
    scheduler.subscribe(Subscription::pane(
        TableKey::filtered(kind, Some(RUNNING)),
        Duration::from_secs(30),
        Some(RUNNING.into()),
        None,
    ));
    // Past the cycle's `Started`, which is now sent *before* the request: the first message
    // that proves the request went out is the page - or the error - that answered it.
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("message")
            .expect("open");
        if !matches!(msg, Msg::Started { .. }) {
            break;
        }
    }
    let sent = pc.requests_to("/config/tasks");
    assert!(
        sent.iter().any(|r| r
            .query
            .iter()
            .any(|(k, v)| k == "$filter" && v.contains("RUNNING"))),
        "{sent:?}"
    );
}

/// A fixture tree holding `rows` tasks: the bundled one has four, and a budget only shows
/// itself over a collection larger than it. Written under the test target's own temp
/// directory, which cargo makes and cleans.
fn many_tasks(rows: usize) -> std::path::PathBuf {
    let list_path = kind("prism.config.Task").unwrap().list_path;
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("tasks-over-the-budget");
    let file = dir.join(format!("{}.json", list_path.trim_start_matches('/')));
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("fixture directory");
    let tasks: Vec<serde_json::Value> = (0..rows)
        .map(|i| {
            serde_json::json!({
                "extId": format!("00000000-0000-0000-0000-{i:012}"),
                "operation": "kVmCreate",
                "operationDescription": "Create VM",
                "status": "SUCCEEDED",
                "createdTime": "2026-09-09T10:00:00.000000Z",
            })
        })
        .collect();
    std::fs::write(&file, serde_json::to_vec(&tasks).expect("json")).expect("fixture file");
    dir
}

/// What this budget is for: 6109 tasks is 62 pages at nearly a second each, on every 3 s
/// cycle, and the first poll never completes. The walk stops at the curated budget, in the
/// curated order, and the total the header shows stays the server's.
#[tokio::test]
async fn a_budgeted_kind_completes_at_the_budget_and_still_reports_the_server_s_total() {
    const ROWS: usize = 620;
    let pc = MockPc::builder().fixtures(many_tasks(ROWS)).start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = TableKey::top(kind("prism.config.Task").unwrap());
    let budget = usize::try_from(key.kind.max_rows.expect("Tasks carry a budget")).unwrap();
    let page_size = 100;
    // Negotiation may have asked for this path already; the cycle's requests start here.
    let before = pc.requests_to(key.kind.list_path).len();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(3600)));
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Complete { .. }), "{msg:?}");

    let table = store.table(&key);
    assert_eq!(table.rows.len(), budget, "the walk stops at the budget");
    assert_eq!(
        table.total,
        Some(ROWS as u64),
        "the server's count, not the budget: the header shows `500/620` the way it shows \
         `500/6109` against a Prism Central"
    );
    let sent = pc.requests_to(key.kind.list_path);
    assert_eq!(
        sent.len() - before,
        budget / page_size,
        "five pages, not seven: {sent:?}"
    );
    // In the curated order, so the rows the budget keeps are the newest ones.
    for r in &sent[before..] {
        assert!(
            r.query
                .iter()
                .any(|(k, v)| k == "$orderby" && v == "createdTime desc"),
            "{:?}",
            r.query
        );
    }
}

/// The other half: a collection smaller than its budget is still walked whole, so the
/// default budget costs the ordinary kinds nothing. VMs curate no budget of their own, so
/// they carry `DEFAULT_MAX_ROWS`, and the mock holds three rows.
#[tokio::test]
async fn a_collection_under_its_budget_is_walked_whole() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    assert_eq!(key.kind.max_rows, Some(DEFAULT_MAX_ROWS));
    scheduler.subscribe(Subscription {
        page_size: 1,
        ..Subscription::list(key.clone(), Duration::from_secs(3600))
    });
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Complete { .. }), "{msg:?}");
    let table = store.table(&key);
    assert_eq!(table.total, Some(3));
    assert_eq!(table.rows.len(), 3, "every row, a page at a time");
    // No curated order on VMs, so none is sent.
    assert!(
        pc.requests_to(VMS)
            .iter()
            .all(|r| r.query.iter().all(|(k, _)| k != "$orderby")),
        "an uncurated kind sends no $orderby"
    );
}

/// And a caller that must have every row asks for it: `max_rows: None` on the subscription
/// still means no budget, whatever the kind carries. Nothing in the TUI opens a list this
/// way today; the escape hatch exists so that a budget is a default, never a ceiling.
#[tokio::test]
async fn an_explicit_none_on_the_subscription_still_means_no_budget() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    scheduler.subscribe(Subscription {
        page_size: 1,
        max_rows: None,
        ..Subscription::list(key.clone(), Duration::from_secs(3600))
    });
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Complete { .. }), "{msg:?}");
    assert_eq!(store.table(&key).rows.len(), 3);
}

// Both hooks take this string, and they mean different things - the client's path and the
// fixture's. They coincide because nothing here calls `serve_versions`.
const AUDITS: &str = "/monitoring/v4.3/serviceability/audits";

const TASKS: &str = "/prism/v4.4/config/tasks";

/// The change-probe's own request: one row, in the probe's order.
fn is_probe(r: &nutsh_mockpc::RecordedRequest) -> bool {
    let q = |name: &str| {
        r.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    q("$limit") == Some("1") && q("$orderby") == Some("lastUpdatedTime desc")
}

/// A slow Prism Central without a sleep: the handler awaits a gate the test opens by hand, so a
/// progress assertion is made against the frames the test chose rather than against a race with
/// the clock. And a five-thousand-row collection without a five-thousand-row file in the
/// repository.
#[tokio::test]
async fn a_held_path_answers_only_what_the_gate_releases() {
    let gate = Gate::new();
    let pc = MockPc::builder()
        .repeat_fixture(AUDITS, 5000)
        .hold_path(AUDITS, &gate)
        .start()
        .await;
    // Negotiation runs against a gate that is already shut, and monitoring's first probe
    // candidate is alerts, which has a fixture and answers: the held audits path is never
    // probed. The timeout says so by name rather than stalling for the client's own minute; the
    // number is loose because the message, not the number, is what makes it worth keeping - a
    // version probe across every namespace is slow on a loaded runner and fast against a gate.
    let client = Arc::new(
        tokio::time::timeout(Duration::from_secs(15), common::client(&pc))
            .await
            .expect("negotiation must not touch the held path"),
    );
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let key = TableKey::top(kind("monitoring.serviceability.Audit").unwrap());
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(60)));

    // The cycle announces itself before it asks for anything, so this arrives through a shut
    // gate - which is the whole point of `Msg::Started`: the frame says `listing…` while the
    // first page is still on the wire.
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a Started")
        .expect("open");
    assert!(matches!(first, Msg::Started { .. }), "{first:?}");

    // Past it, the cycle blocks on the gate: no page arrives.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "the gate is shut"
    );

    gate.release(1);
    let page = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a page")
        .expect("open");
    match page {
        Msg::Page {
            entities, total, ..
        } => {
            assert_eq!(entities.len(), 100);
            assert_eq!(total, Some(5000), "the fixture was blown up to 5000 rows");
            // Fresh extIds, so the store keys them apart rather than collapsing them onto one.
            assert_ne!(entities[0].ext_id, entities[1].ext_id);
        }
        other => panic!("{other:?}"),
    }

    // One permit, one answer: the gate shuts behind the page it let through, and the walk's four
    // remaining pages (`max_rows` is 500) wait where they were.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "the gate shut again"
    );

    gate.release(1);
    let page = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a second page")
        .expect("open");
    match page {
        Msg::Page { entities, .. } => {
            // Row 100 of the repeated fixture, so the second permit resumed the walk where the
            // first one left it rather than starting it over.
            assert_eq!(entities[0].ext_id, "00000000-0000-4000-8000-000000000064");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "one release, one answer"
    );
}

/// The change-probe end to end, against a Prism Central that does not sort - which is exactly
/// what the mock is: it pages the fixture in file order and answers `$orderby` with nothing.
///
/// The first cycle walks and learns the table takes more than one page. The second walks *and*
/// probes - the walk, then one `$limit=1` request ordered by `probe_by`, in that order because
/// a calibrating cycle walks whatever the probe would say and an answer asked last cannot lose
/// a race against the walk beside it. Calibration compares the probe's row against the newest
/// the walk returned and disarms, because the row this Prism Central handed back is older: it
/// paged the fixture in file order and answered the order it was asked for with nothing. The
/// third cycle walks and probes nothing, for ever after.
#[tokio::test]
async fn the_probe_costs_one_request_once_and_a_pc_that_ignores_orderby_disarms_it() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = TableKey::top(kind("prism.config.Task").unwrap());
    assert_eq!(
        key.kind.probe_by,
        Some("lastUpdatedTime"),
        "Tasks are armed"
    );
    // Negotiation may have asked for this path already; the cycle's requests start here.
    let before = pc.requests_to(TASKS).len();
    scheduler.subscribe(Subscription {
        page_size: 1,
        ..Subscription::list(key.clone(), Duration::from_secs(1))
    });
    // The requests this subscription made past the first `n` of them.
    let since = |n: usize| -> Vec<nutsh_mockpc::RecordedRequest> {
        pc.requests_to(TASKS).into_iter().skip(before + n).collect()
    };

    until_settled(&mut rx, &mut store, &key).await;
    // Four rows, one a page: four requests, and no probe before a walk has shown the table
    // takes more than one page.
    let first = since(0);
    assert_eq!(first.len(), 4);
    assert!(first.iter().all(|r| !is_probe(r)), "nothing to probe yet");
    assert_eq!(store.table(&key).rows.len(), 4);

    until_settled(&mut rx, &mut store, &key).await;
    let second = since(4);
    assert_eq!(
        second.len(),
        5,
        "four pages and the one probe that could not save them"
    );
    assert!(
        is_probe(&second[4]),
        "the calibrating cycle probes last: {second:?}"
    );
    assert!(second[..4].iter().all(|r| !is_probe(r)));

    until_settled(&mut rx, &mut store, &key).await;
    let third = since(9);
    assert_eq!(third.len(), 4, "disarmed: a walk, and never a probe again");
    assert!(third.iter().all(|r| !is_probe(r)));
    assert_eq!(
        store.table(&key).rows.len(),
        4,
        "and the rows are still there"
    );
}

/// The other half, and the whole point of the probe: a Prism Central that *does* sort. The
/// mock honours `$orderby` only when a test asks it to, because a PC that ignores it is a real
/// one and the test above is that PC.
///
/// Cycle one walks the four one-row pages. Cycle two walks, probes, and arms: the probe's row
/// is the newest by `lastUpdatedTime` that the walk returned. The nine after it cost one
/// request each - the rows, the total and the generation stay exactly where they were, while
/// `last_poll` moves and the error stays clear, which is what keeps the header green through a
/// skip. The tenth walks again whatever the probe says, because `MAX_SKIPS` is the bound on
/// anything the comparison structurally cannot see.
#[tokio::test]
async fn an_armed_probe_skips_nine_cycles_and_then_walks() {
    let pc = MockPc::builder().honours_orderby().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let mut store = Store::default();
    let key = TableKey::top(kind("prism.config.Task").unwrap());
    // Negotiation may have asked for this path already; the cycle's requests start here.
    let before = pc.requests_to(TASKS).len();
    scheduler.subscribe(Subscription {
        page_size: 1,
        ..Subscription::list(key.clone(), Duration::from_secs(1))
    });
    let since = |n: usize| -> Vec<nutsh_mockpc::RecordedRequest> {
        pc.requests_to(TASKS).into_iter().skip(before + n).collect()
    };

    until_settled(&mut rx, &mut store, &key).await;
    assert_eq!(since(0).len(), 4, "four rows, one a page");
    until_settled(&mut rx, &mut store, &key).await;
    let arming = since(4);
    assert_eq!(
        arming.len(),
        5,
        "the walk that calibrates it, then the probe"
    );
    assert!(is_probe(&arming[4]), "{arming:?}");

    // What a skip must leave untouched, taken from the last cycle that walked.
    let armed = store.table(&key);
    let rows: Vec<String> = armed.rows.keys().cloned().collect();
    let total = armed.total;
    let generation = armed.generation;
    let mut last_poll = armed.last_poll.expect("a completed cycle");
    assert_eq!(rows.len(), 4);

    for skip in 1..=9usize {
        until_settled(&mut rx, &mut store, &key).await;
        // Nine requests are behind us: the first cycle's four and the arming cycle's five.
        let cycle = since(8 + skip);
        assert_eq!(cycle.len(), 1, "skip {skip} is one request");
        assert!(is_probe(&cycle[0]), "skip {skip}: {cycle:?}");
        let table = store.table(&key);
        assert_eq!(
            table.rows.keys().cloned().collect::<Vec<_>>(),
            rows,
            "skip {skip} staged nothing, so the rows are the walk's"
        );
        assert_eq!(table.total, total, "skip {skip}");
        assert_eq!(table.generation, generation, "no cycle replaced the rows");
        assert!(table.error.is_none(), "skip {skip}");
        let polled = table.last_poll.expect("the skip completed");
        assert!(polled > last_poll, "skip {skip} refreshed `last_poll`");
        last_poll = polled;
    }

    // Nine skips of a four-page walk against the nine probes they cost: 36 requests avoided
    // out of the 50 this table could have spent, which is the counter pair the header's
    // `cached` reads. A skip that recorded one fewer would shrink both halves of that ratio -
    // 27 avoided against 41 - and read 0.66 here.
    let meter = client.metrics().snapshot(std::time::Instant::now());
    let cached = meter.cached.expect("something avoidable happened");
    assert!((0.70..=0.73).contains(&cached), "{cached}");

    until_settled(&mut rx, &mut store, &key).await;
    let tenth = since(18);
    assert_eq!(tenth.len(), 5, "the cap walks: {tenth:?}");
    assert!(
        is_probe(&tenth[0]),
        "an armed cycle probes first: {tenth:?}"
    );
    assert!(tenth[1..].iter().all(|r| !is_probe(r)));
    assert_eq!(store.table(&key).rows.len(), 4);
}

/// Why the calibrating cycle walks before it probes. A probe issued first is a request older
/// than the walk beside it, so an entity updated while the walk pages carries a value newer
/// than the probe's row through no fault of the Prism Central's - and calibration would read
/// that as a PC that does not sort, on the one cycle whose verdict stands for the whole
/// session. On Tasks the race is not hypothetical: the 500 newest-created rows the budget keeps
/// are exactly the ones whose `lastUpdatedTime` churns, and a five-page walk is a five-second
/// window over them.
///
/// Here the Prism Central sorts, and one row is edited after the walk's last page and before
/// the probe. The table must arm anyway, and the cycle after it must cost one request.
#[tokio::test]
async fn a_row_edited_during_the_calibrating_walk_still_arms_the_table() {
    // The oldest of the four fixture tasks by `createdTime`, so the walk - ordered by creation,
    // descending - reads it on its last page, and the edit lands behind that answer.
    const EDITED: &str = "ZXJnb24=:11111111-1111-4111-8111-000000000002";
    // Newer than every `lastUpdatedTime` in the fixture, the newest of which is 09:59:50Z.
    const EDITED_AT: &str = "2026-09-05T10:02:00Z";

    let pc = MockPc::builder().honours_orderby().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = TableKey::top(kind("prism.config.Task").unwrap());
    let before = pc.requests_to(TASKS).len();
    scheduler.subscribe(Subscription {
        page_size: 1,
        ..Subscription::list(key.clone(), Duration::from_secs(1))
    });
    let since = |n: usize| -> Vec<nutsh_mockpc::RecordedRequest> {
        pc.requests_to(TASKS).into_iter().skip(before + n).collect()
    };

    until_settled(&mut rx, &mut store, &key).await;
    assert_eq!(since(0).len(), 4, "four rows, one a page");

    // Armed at the exact request the race needs: the fourth answer of the cycle that follows is
    // the walk's last page, and the edit lands between it and the probe. Every row the walk
    // returned carries its old value; only the probe can see the new one.
    pc.mutate_after_list(TASKS, EDITED, "lastUpdatedTime", EDITED_AT, 4);

    until_settled(&mut rx, &mut store, &key).await;
    let arming = since(4);
    assert_eq!(arming.len(), 5, "the walk, then the probe");
    assert!(is_probe(&arming[4]), "{arming:?}");

    // Armed: the next cycle is the one request the probe exists to make. Probing first would
    // have compared a row read before the edit against a walk that saw it, and turned the probe
    // off for the session on a Prism Central that sorted perfectly.
    until_settled(&mut rx, &mut store, &key).await;
    let skipped = since(9);
    assert_eq!(
        skipped.len(),
        1,
        "armed, so the walk is skipped: {skipped:?}"
    );
    assert!(is_probe(&skipped[0]));
    assert_eq!(
        store.table(&key).rows.len(),
        4,
        "and the rows are still the walk's"
    );
}

/// A 404 on a *list*, asked twice, is the same answer every cycle - the path is not on this
/// Prism Central - so the table is marked and the subscription stops, rather than a retry loop
/// running for ever behind a screen nobody is watching.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_404_on_a_list_marks_the_table_and_stops_the_subscription() {
    let pc = MockPc::builder().missing_path(VMS).start().await;
    let mut store = Store::default();
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(Arc::new(common::client(&pc).await), tx);
    let key = vms();
    let sub = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(200)));

    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(
            msg,
            Msg::Error { ref error, .. } if error.is_not_served()
        ),
        "{msg:?}"
    );
    assert!(store.table(&key).not_served);
    // Two requests, one cycle: the second is the lazy pin repair's chance to have changed the
    // route, and the view is told once, after it. See the test below.
    assert_eq!(pc.requests_to(VMS).len(), 2);
    // And no third: the task returned rather than backing off. The interval floors to a
    // second, so two are more than enough to have seen one.
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .is_err(),
        "a 404 on a list does not retry"
    );

    // `^r` is a person, not "a screen nobody is watching": it starts the stopped task again,
    // and it costs the same two requests as any other cycle over a path that is not there.
    scheduler.refresh(sub);
    let again = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(
            again,
            Msg::Error { ref error, .. } if error.is_not_served()
        ),
        "{again:?}"
    );
    assert_eq!(pc.requests_to(VMS).len(), 4);
}

/// The 404 the *pin* is to blame for, not the endpoint: a restored pin routes vmm at a version
/// this Prism Central does not serve, `Client::repair_after` re-negotiates that namespace
/// inside the failed call itself, and the attempt behind it is the one that answers. Stopping
/// on the first 404 would strand a kind the PC serves perfectly well - no rows, a greyed menu
/// item and no polling, for the rest of the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_list_404_the_repaired_pin_answers_is_not_a_not_served() {
    let pc = MockPc::builder()
        .serve_versions("vmm", &["v4.3"])
        .start()
        .await;
    let client = Client::connect(
        &Profile {
            host: pc.host(),
            port: pc.port(),
            username: "admin".into(),
            verify_tls: true,
            ca_bundle: None,
            plain_http: true,
        },
        "secret",
    )
    .unwrap();
    // A cache's pins, adopted without a single request: vmm at a version that 404s.
    client.adopt_pins(
        &BTreeMap::from([("vmm".to_string(), "v4.0".to_string())]),
        vec![NamespaceStatus {
            name: "vmm",
            version: "v4.3",
            pinned: Some("v4.0"),
            ok: true,
            detail: "probed /vmm/v4.0/ahv/config/vms".into(),
            restored: true,
        }],
    );
    let mut store = Store::default();
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(Arc::new(client), tx);
    let key = vms();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(200)));

    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Complete { .. }), "{msg:?}");
    let t = store.table(&key);
    assert!(!t.not_served, "the endpoint is there, at the repaired pin");
    assert_eq!(t.rows.len(), 3);
    assert_eq!(pc.requests_to(VMS).len(), 1, "the retry, at v4.3");
}

/// Any other failure is the service or the network, not the endpoint's absence, and is retried
/// exactly as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_500_is_not_a_not_served() {
    let pc = MockPc::builder().fail_path(VMS, 500).start().await;
    let mut store = Store::default();
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(Arc::new(common::client(&pc).await), tx);
    let key = vms();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(200)));

    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(
            msg,
            Msg::Error { ref error, .. } if !error.is_not_served()
        ),
        "{msg:?}"
    );
    assert!(!store.table(&key).not_served);
    // And it comes round again.
    let second = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(second, Msg::Error { .. }), "{second:?}");
}

/// A 404 on a **single entity** is one row that has gone, not an endpoint that is not there, so
/// it is an ordinary error and the subscription lives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_404_on_one_entity_is_an_ordinary_error() {
    let pc = MockPc::builder().start().await;
    let mut store = Store::default();
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(Arc::new(common::client(&pc).await), tx);
    let key = vms();
    scheduler.subscribe(Subscription {
        single: Some("3d0c4a2e-1b8f-4c1a-9e2f-0000000000ff".to_string()),
        ..Subscription::list(key.clone(), Duration::from_millis(200))
    });
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(
        matches!(
            msg,
            Msg::Error { ref error, .. } if !error.is_not_served()
        ),
        "{msg:?}"
    );
    assert!(!store.table(&key).not_served);
}

/// A paused task waits on the `Notify` it already holds instead of sleeping its interval - one
/// branch around the existing `select!`, no new task, channel or message.
///
/// The task watch subscribed beside the list is the exemption, and the reason the cycles are
/// counted per subscription rather than in aggregate: it is the user's own pending action, and
/// pausing it would strand a mutation somebody is waiting on with nothing on screen saying so.
#[tokio::test(start_paused = true)]
async fn a_paused_subscription_issues_nothing_until_it_is_woken() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let key = vms();
    let list = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(5)));
    // A watch is a `single` that says so - `watching()` is the only thing that tells the two
    // apart - and one second is its interval here, so a cycle of it is a second to wait out.
    let watch = scheduler.subscribe(
        Subscription::single(key.clone(), Duration::from_secs(1), WEB01.into()).watching(),
    );
    // Both have finished a cycle, so neither is mid-cycle when the pause is set.
    let mut settled = std::collections::HashSet::new();
    while settled.len() < 2 {
        settled.insert(next_complete(&mut rx).await);
    }

    scheduler.set_idle(true);
    let listed = pc.requests_to(VMS).len();
    tokio::time::advance(Duration::from_secs(600)).await;
    tokio::task::yield_now().await;
    assert_eq!(pc.requests_to(VMS).len(), listed, "nobody is reading it");

    // The watch keeps its own clock. On a real one for this half: waiting out a cycle under the
    // paused clock would race its auto-advance rather than time anything.
    tokio::time::resume();
    for _ in 0..2 {
        let sub = tokio::time::timeout(Duration::from_secs(10), next_complete(&mut rx))
            .await
            .expect("a task watch never pauses");
        assert_eq!(
            sub, watch,
            "and while the pause holds it is the only one cycling"
        );
    }
    assert_eq!(pc.requests_to(VMS).len(), listed, "still nobody reading it");

    // Waking runs a cycle immediately, and with a forced walk.
    scheduler.set_idle(false);
    scheduler.refresh_all();
    tokio::time::timeout(Duration::from_secs(10), async {
        while next_complete(&mut rx).await != list {}
    })
    .await
    .expect("waking runs a cycle now");
    assert!(pc.requests_to(VMS).len() > listed);
    scheduler.unsubscribe(list);
    scheduler.unsubscribe(watch);
}

/// How the detail was decoupled from the list row, which is what let `$select` ship.
///
/// The detail pane's `Subscription::single` shares the `TableKey` of the table it sits over,
/// and its whole `get_in` document lands under that key. A list cycle's `Complete` replaces
/// `rows` **wholesale** with the entities it staged, and nothing pauses the list under an open
/// detail, so while the pane was composed from `rows` a narrowing `$select` would have made it
/// alternate between the whole document and the projected one for as long as it was open.
///
/// It is composed from `Table::row` now, which prefers the document a single-entity GET
/// fetched. This test is the old hazard, kept: `rows` still loses the whole row on every
/// cycle, exactly as it did, and `row` is what no longer does.
#[test]
fn a_list_cycle_takes_the_whole_row_off_rows_and_leaves_it_on_the_detail() {
    let key = vms();
    let mut store = Store::default();
    let whole = |id: &str| {
        nutsh_prism::Entity::new(
            key.kind,
            serde_json::json!({"extId": id, "name": "web-01", "description": "the whole row"}),
            None,
        )
    };
    // What a `$select` narrowed to the columns would return for the same row.
    let projected = |id: &str| {
        nutsh_prism::Entity::new(
            key.kind,
            serde_json::json!({"extId": id, "name": "web-01"}),
            None,
        )
    };
    let list_cycle = |store: &mut Store, generation: u64| {
        store.apply(
            &key,
            Update::Page {
                generation,
                entities: vec![projected(WEB01)],
                total: Some(1),
            },
        );
        store.apply(&key, Update::Complete { generation });
    };
    let described = |store: &Store| {
        store.table(&key).rows[WEB01]
            .raw
            .get("description")
            .is_some()
    };
    // What the detail pane actually reads.
    let detail_described = |store: &Store| {
        store
            .table(&key)
            .row(WEB01)
            .expect("the row is there")
            .raw
            .get("description")
            .is_some()
    };

    list_cycle(&mut store, 1);
    assert!(
        !described(&store),
        "a projected list row carries no description"
    );

    // The detail opens and its `get_in` lands: the row is whole, and the pane is right.
    store.apply(
        &key,
        Update::Entity {
            generation: 2,
            entity: whole(WEB01),
        },
    );
    assert!(described(&store), "the single's document reached the store");

    // One list cycle later `rows` holds the projection again, as it always did.
    list_cycle(&mut store, 3);
    assert!(
        !described(&store),
        "a list `Complete` replaces the rows with what it staged"
    );
    // And the pane is still whole, which is the whole of why `$select` is safe.
    assert!(
        detail_described(&store),
        "the detail is composed from the document its own GET fetched"
    );
}

/// The 404 is the server's verdict about an *endpoint*, so it is remembered on the client and
/// not only on the table that paid for it: the same kind is also a pane on a page, a palette
/// row, a menu item and a stats counter, and each of those would otherwise re-learn the same
/// 404 once a cycle of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_list_404_is_remembered_on_the_client() {
    let pc = MockPc::builder().missing_path(VMS).start().await;
    let client = Arc::new(common::client(&pc).await);
    let vm = kind("vmm.ahv.config.Vm").unwrap();
    assert!(
        client.is_served(vm),
        "nothing has answered yet, which is a reason to ask"
    );
    let mut store = Store::default();
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let key = vms();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(200)));
    until_settled(&mut rx, &mut store, &key).await;

    assert_eq!(client.list_availability(vm), Availability::ListNotFound);
    assert!(!client.is_served(vm));
    assert_eq!(
        client.availability(vm),
        Availability::Served,
        "and the person who opens the table anyway still gets it, and `^r` with it"
    );
    assert_eq!(
        client.list_availability(vm).reason().as_deref(),
        Some("not served by this Prism Central (HTTP 404)"),
        "and every reader of it says what the table says"
    );
}

/// And a cycle that completes forgets it: the way back for `^r`, and for a service that was
/// still starting when the session opened. Marked by hand because the mock's missing paths are
/// fixed at start-up - what is under test is the scheduler's clearing, not the mock's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completed_cycle_forgets_a_remembered_404() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let vm = kind("vmm.ahv.config.Vm").unwrap();
    client.mark_missing(vm);
    assert!(!client.is_served(vm));
    let mut store = Store::default();
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let key = vms();
    scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(200)));
    until_settled(&mut rx, &mut store, &key).await;

    assert_eq!(client.list_availability(vm), Availability::Served);
    assert!(client.is_served(vm));
}

/// A credential this Prism Central refused stops the subscription, exactly as a 404 on the
/// endpoint does - and for a sharper reason. A 404 retried for ever is wasted requests; a
/// password retried for ever is failures counted against a real account, and this program has
/// locked out a real administrator twice.
///
/// The rows and the reason stay on the table. What stops is the asking.
#[tokio::test(start_paused = true)]
async fn a_refused_credential_stops_the_subscription() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::raw_client(&pc, "wrong"));
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    let _sub = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(200)));

    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Error { .. }), "{msg:?}");
    let failure = store.table(&key).error.clone().expect("a failure");
    assert!(failure.is_terminal(), "{failure:?}");
    assert_eq!(pc.requests().len(), 1, "one presentation");

    // Half an hour of cycles at two hundred milliseconds. Not one of them goes out.
    tokio::time::advance(Duration::from_secs(1800)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        pc.requests().len(),
        1,
        "a refused password is never presented again"
    );
    // And the task itself is gone. `Client::send` would refuse to put the password on the wire
    // whatever this loop did, so the request count alone cannot tell a stopped subscription
    // from one spinning against the gate; a subscription still cycling reports a failure every
    // two hundred milliseconds, and nine thousand of them have now had their chance.
    assert!(
        rx.try_recv().is_err(),
        "the subscription is still reporting failures"
    );
}

/// The same when the 401 arrives mid-session, on one endpoint of a Prism Central that had been
/// answering: the subscription stops there too, and the rows it already had stay drawn.
/// On a real clock, unlike its sibling above: this session negotiates first, and a paused clock
/// races ahead of the rate budget's own - which is a `std::time::Instant` - so the budget never
/// refills and the cycle under test never gets a token. `MIN_INTERVAL` is a second and the
/// first back-off is the interval, so two seconds of wall time is one cycle that a subscription
/// which had not stopped would certainly have run.
#[tokio::test]
async fn a_401_midway_stops_the_subscription_and_keeps_the_rows() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let mut store = Store::default();
    let key = vms();
    let _sub = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(200)));

    let first = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(first, Msg::Complete { .. }), "{first:?}");
    assert_eq!(store.table(&key).rows.len(), 3);

    pc.fail_from_now(VMS, 401);
    let msg = until_settled(&mut rx, &mut store, &key).await;
    assert!(matches!(msg, Msg::Error { .. }), "{msg:?}");
    assert!(store.table(&key).error.as_ref().unwrap().is_terminal());
    assert_eq!(store.table(&key).rows.len(), 3, "the stale rows stay drawn");

    let spent = pc.requests().len();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(pc.requests().len(), spent, "the asking stopped");
    assert!(
        rx.try_recv().is_err(),
        "the subscription is still reporting failures"
    );
}

/// The seven header counters are not subscriptions either, and they run every thirty seconds.
/// A refused credential ends them, rather than blanking each counter and coming back for more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_credential_ends_the_header_counters() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    // The first counter's kind answers 401, so the refusal lands on the first of the seven.
    pc.fail_from_now(kind("clustermgmt.config.Cluster").unwrap().list_path, 401);
    let spent = presentations(&pc);

    let (tx, _rx) = mpsc::channel(64);
    let handle = nutsh_core::stats::spawn(Arc::clone(&client), tx, 1, None);

    for _ in 0..200 {
        if handle.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(handle.is_finished(), "the counters are still asking");
    assert!(client.auth_rejected());
    assert_eq!(
        presentations(&pc) - spent,
        1,
        "one presentation, not seven every thirty seconds"
    );
}

/// And the Disaster Recovery sampler, which asks about fifty VMs one at a time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_credential_ends_the_dr_sampler() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    // The first thing a cycle counts answers 401.
    pc.fail_from_now(
        kind("datapolicies.config.ProtectionPolicy")
            .unwrap()
            .list_path,
        401,
    );
    let spent = presentations(&pc);

    let (tx, _rx) = mpsc::channel(64);
    let handle = nutsh_core::sampler::spawn(
        Arc::clone(&client),
        tx,
        1,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );

    for _ in 0..200 {
        if handle.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(handle.is_finished(), "the sampler is still asking");
    assert!(client.auth_rejected());
    assert_eq!(
        presentations(&pc) - spent,
        1,
        "one presentation, not four counters and fifty gets"
    );
}
/// How many of the recorded requests carried a credential rather than riding the session
/// cookie. The number a Prism Central with a lockout policy is counting.
fn presentations(pc: &MockPc) -> usize {
    pc.requests().iter().filter(|r| r.authenticates()).count()
}

/// The scheduler is the only thing that says `$select`, and it says it on the cycle that fills
/// a table.
///
/// The distinction is the safety of the whole feature. `can_i` lists the authorisation
/// policies through `Client::list_page` with default options, and so do the version probes; a
/// `$select` applied inside the client from `Kind::select` would reach both. It would drop
/// `identities[].identityFilter` from the policies, which is the field the role resolution
/// reads, and every action on every kind would grey as not permitted without a word on screen.
#[tokio::test]
async fn a_list_cycle_carries_the_kinds_select_and_nothing_else_does() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let vm = kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
    let select = vm.select.expect("VMs are narrowed");

    let sub = scheduler.subscribe(Subscription::list(vms(), Duration::from_secs(60)));
    next_complete(&mut rx).await;
    scheduler.unsubscribe(sub);

    let lists = pc.requests_to(VMS);
    assert!(!lists.is_empty(), "the cycle made no list request");
    for r in &lists {
        let sent: Vec<&str> = r
            .query
            .iter()
            .filter(|(k, _)| k == "$select")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(sent, [select], "{:?}", r.query);
    }

    // Everything else that lists a collection goes out whole.
    let _ = nutsh_core::can_i::resolve(&client, "admin").await;
    for r in pc.requests() {
        if r.path.ends_with(VMS) {
            continue;
        }
        assert!(
            !r.query.iter().any(|(k, _)| k == "$select"),
            "{} carried a $select: {:?}",
            r.path,
            r.query
        );
    }
}

/// A Prism Central that refuses the narrowing keeps its table.
///
/// `list_params.select` is what the spec file declares, and a deployment is free to disagree
/// with it. A 400 is the endpoint saying so, and the answer is to ask for the whole document
/// instead - once, remembered for the session, so the next cycle does not spend a request
/// rediscovering it.
#[tokio::test]
async fn a_four_hundred_on_a_narrowed_list_gives_up_the_narrowing_and_keeps_the_rows() {
    let pc = MockPc::builder().reject_select(VMS).start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client.clone(), tx);
    let mut store = Store::default();
    let key = vms();

    let sub = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_millis(50)));
    until_settled(&mut rx, &mut store, &key).await;
    assert!(
        store.table(&key).error.is_none(),
        "{:?}",
        store.table(&key).error
    );
    assert!(!store.table(&key).rows.is_empty(), "the table has its rows");

    let first: Vec<bool> = pc
        .requests_to(VMS)
        .iter()
        .map(|r| r.query.iter().any(|(k, _)| k == "$select"))
        .collect();
    assert_eq!(
        first,
        [true, false],
        "one refused attempt, then the whole document"
    );
    assert!(client.select_refused(key.kind), "the give-up is remembered");

    // And the next cycle does not try again.
    until_settled(&mut rx, &mut store, &key).await;
    scheduler.unsubscribe(sub);
    assert!(
        pc.requests_to(VMS)
            .iter()
            .skip(2)
            .all(|r| !r.query.iter().any(|(k, _)| k == "$select")),
        "a later cycle asked again"
    );
}

/// Nothing further arrives. **Real time, not a paused clock**: a cycle that should not have run
/// still has to reach a socket and come back, so advancing a frozen clock and yielding once
/// would find the request count unchanged whether the guard works or not. Three hundred
/// milliseconds is two orders of magnitude more than a round trip to the mock in-process.
async fn nothing_more(rx: &mut mpsc::Receiver<Msg>) {
    let ran = tokio::time::timeout(Duration::from_millis(300), next_complete(rx)).await;
    assert!(ran.is_err(), "a cycle ran that nothing asked for");
}

/// A schedule changed while the task is asleep is honoured on the wake, not one cycle later -
/// and `off` never costs a last cycle nobody asked for. Then `ctrl-r` on a task with no
/// schedule runs **exactly one** cycle, because that is where the loop goes back to.
#[tokio::test(start_paused = true)]
async fn off_stops_at_once_and_a_refresh_then_buys_exactly_one_cycle() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let id = scheduler.subscribe(Subscription::list(vms(), Duration::from_secs(5)));
    while next_complete(&mut rx).await != id {}

    // Asleep on its five seconds. `off` while it sleeps must not buy one more cycle.
    assert!(scheduler.set_interval(id, None));
    assert!(scheduler.is_manual(id));
    let after = pc.requests_to(VMS).len();
    tokio::time::advance(Duration::from_secs(600)).await;
    tokio::time::resume();
    nothing_more(&mut rx).await;
    assert_eq!(pc.requests_to(VMS).len(), after, "no schedule, no cycles");

    // `ctrl-r`: one cycle, and the schedule is still off.
    scheduler.refresh(id);
    while next_complete(&mut rx).await != id {}
    let once = pc.requests_to(VMS).len();
    assert!(once > after);
    nothing_more(&mut rx).await;
    assert_eq!(pc.requests_to(VMS).len(), once, "exactly one");
    assert!(scheduler.is_manual(id), "and still manual");

    // A named interval arms, so the rhythm starts with a cycle rather than with silence.
    assert!(scheduler.set_interval(id, Some(Duration::from_secs(5))));
    while next_complete(&mut rx).await != id {}
    assert!(!scheduler.is_manual(id));
    scheduler.unsubscribe(id);
}

/// `set_interval_kind` reaches every pausable subscription over the kind and no other. A task
/// watch is the user's own pending mutation and is not one of them - the same
/// `Subscription::pausable` the idle pause reads, so the two features exempt the same pollers
/// by construction rather than by two lists.
#[tokio::test(start_paused = true)]
async fn set_interval_kind_leaves_a_task_watch_alone() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, _rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let key = vms();
    let list = scheduler.subscribe(Subscription::list(key.clone(), Duration::from_secs(5)));
    let watch = scheduler
        .subscribe(Subscription::single(key, Duration::from_secs(5), WEB01.into()).watching());

    assert_eq!(
        scheduler.set_interval_kind("vmm.ahv.config.Vm", None),
        1,
        "the list only"
    );
    assert!(!scheduler.is_manual(watch), "a watch keeps its own rhythm");
    assert!(scheduler.is_manual(list));
    scheduler.unsubscribe(list);
    scheduler.unsubscribe(watch);
}

/// The idle pause's wake skips a view the user turned off: without the skip, one keystroke
/// after five idle minutes would refresh a table that is supposed to be silent.
#[tokio::test(start_paused = true)]
async fn waking_from_the_pause_leaves_a_manual_view_alone() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let mut scheduler = Scheduler::new(client, tx);
    let id = scheduler.subscribe(Subscription::list(vms(), Duration::from_secs(5)));
    while next_complete(&mut rx).await != id {}
    scheduler.set_interval(id, None);
    scheduler.set_idle(true);
    let before = pc.requests_to(VMS).len();

    scheduler.set_idle(false);
    scheduler.refresh_all();
    tokio::time::resume();
    nothing_more(&mut rx).await;
    assert_eq!(pc.requests_to(VMS).len(), before, "still nothing polling");
    scheduler.unsubscribe(id);
}
