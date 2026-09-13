mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use nutsh_core::scheduler::Msg;
use nutsh_core::store::Store;
use nutsh_mockpc::MockPc;
use tokio::sync::mpsc;

const CLUSTER: &str = "0006158a-2f0d-4d5a-8e2d-000000000010";
const HOST: &str = "7b2f2f70-0f6a-4b58-9f9b-000000000020";
const CONTAINER: &str = "5f6e7d8c-0000-4000-8000-000000000201";
const PC: &str = "18b7c9d0-4e1a-4c2b-9f3d-000000000030";

/// A session with somebody at it: the flag every warm-up below takes but the paused one's.
fn awake() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// The first `Msg::Names` of the session, or a panic: the cycle paces its requests 200 ms
/// apart, so ten kinds take about two seconds and the timeout is generous.
async fn first_cycle(rx: &mut mpsc::Receiver<Msg>) -> Vec<(String, String)> {
    let msg = tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("a warm-up cycle within 20 s")
        .expect("the channel is open");
    match msg {
        Msg::Names { names, .. } => names,
        other => panic!("expected Msg::Names, got {other:?}"),
    }
}

/// One cycle fills the cache with every warmed kind the mock serves, without any table having
/// been polled - which is the whole point: `nutsh vm --snapshot` subscribes to VMs and nothing
/// else, so without it every `cluster.extId` and `host.extId` falls through to a stub.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_cycle_names_the_reference_kinds() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let _handle = nutsh_core::names::spawn(client, tx, 1, awake());

    let names = first_cycle(&mut rx).await;
    let mut store = Store::default();
    store.apply_names(names);
    assert_eq!(store.names().get(CLUSTER), Some("lab-cluster"));
    assert_eq!(store.names().get(HOST), Some("ahv-node-1"));
    assert_eq!(store.names().get(CONTAINER), Some("default-container"));
    // `prism.config.DomainManager` has no top-level name at all: this is `name_path` working
    // end to end, through the poller, into the cache.
    assert_eq!(store.names().get(PC), Some("pc-lab"));
}

/// A kind whose namespace this Prism Central does not serve is skipped without a request, and
/// the others still land: one missing namespace must not cost the whole cycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unserved_namespace_does_not_stop_the_cycle() {
    let pc = MockPc::builder()
        .unavailable_namespace("prism")
        .start()
        .await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let _handle = nutsh_core::names::spawn(client, tx, 1, awake());

    let names = first_cycle(&mut rx).await;
    let ids: Vec<&str> = names.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&CLUSTER), "{ids:?}");
    assert!(!ids.contains(&PC), "prism is not served: {ids:?}");
}

/// Names are last-write-wins and nothing is ever removed: both sources read the entity live,
/// so neither is staler than the other, and a name that came from a table the user has since
/// left is still the right name.
#[tokio::test]
async fn apply_names_overwrites_and_never_removes() {
    let mut store = Store::default();
    store.apply_names(vec![("a".into(), "one".into()), ("b".into(), "two".into())]);
    store.apply_names(vec![("a".into(), "ONE".into())]);
    assert_eq!(store.names().get("a"), Some("ONE"));
    assert_eq!(store.names().get("b"), Some("two"), "not removed");
}

/// Dropping the handle stops the poller: `Live` owns it and `App::attach` replaces `Live`
/// wholesale on a context switch, so a task that outlived the session would drop the previous
/// Prism Central's names into the new store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handle_aborts_the_poller() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    // A second sender, held for the length of the test the way `App` holds `poll_tx` for the
    // length of the process: without it the abort would drop the only sender and `recv` would
    // return `None` at once, which proves the channel closed rather than the poller stopped.
    let _tx = tx.clone();
    let handle = nutsh_core::names::spawn(client, tx, 1, awake());
    let _ = first_cycle(&mut rx).await;
    handle.abort();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "nothing more arrives once the handle is aborted"
    );
}

/// Five idle minutes stop the warm-up too: it is a bare `tokio::spawn` like the Disaster
/// Recovery sampler, not a `Subscription`, so it consults the scheduler's flag itself. A page
/// of every warmed kind, for a cache nobody is reading a reference out of, is exactly what the
/// pause is for - and without this the idle floor would be higher than the plan's arithmetic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pause_stops_the_warm_up() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, mut rx) = mpsc::channel(64);
    let _tx = tx.clone();
    // The requests are what is asserted, not the message: one cycle sends a single
    // `Msg::Names` only after walking every warmed kind, `PACE` apart, so no timeout short
    // enough for a test proves anything about the message. The first `list_page` has no
    // pacing ahead of it and lands well inside this one.
    let before = pc.requests().len();
    let handle = nutsh_core::names::spawn(client, tx, 1, Arc::new(AtomicBool::new(true)));
    assert!(
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .is_err(),
        "no cycle while the session is idle"
    );
    assert_eq!(pc.requests().len(), before, "and not one request either");
    // The cycle is what is skipped, not the poller: the flag is read again next period.
    assert!(!handle.is_finished());
}

/// A closed channel ends the poller even while the pause holds: the send is the exit the loop
/// usually takes, and a paused cycle never reaches it, so the receiver is looked for at the top
/// instead. The abort in `impl Drop for Live` is what ends this task in practice - this is the
/// end it can reach on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_channel_ends_the_paused_poller() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    let (tx, rx) = mpsc::channel(64);
    drop(rx);
    let handle = nutsh_core::names::spawn(client, tx, 1, Arc::new(AtomicBool::new(true)));
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("it returns rather than sleeping out the period")
        .expect("and returns rather than panicking");
}

/// The warm-up is not a subscription, so nothing of the scheduler's stops it. It has to stop
/// itself: ten kinds a cycle, every five minutes, each one a password on the wire, is how an
/// account gets locked by a program nobody is even looking at.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_credential_ends_the_warm_up() {
    let pc = MockPc::builder().start().await;
    let client = Arc::new(common::client(&pc).await);
    // The first kind the warm-up asks for answers 401, so the refusal is learned on the first
    // request of the first cycle - which is where it would be learned in the field too.
    let warm = nutsh_catalog::KINDS
        .iter()
        .find(|k| k.warm)
        .expect("the catalog warms some kinds");
    pc.fail_from_now(warm.list_path, 401);
    let spent = presentations(&pc);

    let (tx, _rx) = mpsc::channel(64);
    let handle = nutsh_core::names::spawn(Arc::clone(&client), tx, 1, awake());
    for _ in 0..200 {
        if handle.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(handle.is_finished(), "the warm-up is still asking");
    assert!(client.auth_rejected());
    assert_eq!(
        presentations(&pc) - spent,
        1,
        "one presentation, not one per warmed kind per cycle for the life of the session"
    );
}
/// How many of the recorded requests carried a credential rather than riding the session
/// cookie. The number a Prism Central with a lockout policy is counting.
fn presentations(pc: &MockPc) -> usize {
    pc.requests().iter().filter(|r| r.authenticates()).count()
}
