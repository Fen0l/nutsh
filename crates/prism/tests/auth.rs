//! A credential this Prism Central has refused is never presented again.
//!
//! The rule this suite pins is one sentence: a 401 on a request that carried a credential means
//! the credential is wrong, and a wrong credential does not become right by being sent again.
//! Everything a Prism Central can do about a password presented over and over - count it, lock
//! the account it belongs to - this program has to make structurally impossible rather than
//! merely unlikely. It has locked out a real admin account twice.

use std::sync::Arc;
use std::time::Duration;

use nutsh_catalog::{Kind, kind};
use nutsh_mockpc::{Gate, MockPc};
use nutsh_prism::valve;
use nutsh_prism::{Client, ListOptions, PrismError, Profile};

const VMS: &str = "/vmm/v4.3/ahv/config/vms";
const OTHER: &str = "/clustermgmt/v4.1/config/clusters";
/// One VM by ext id: a path `get_path` can both reach and decode, which is what a test that
/// asserts on the *answers* needs rather than only on the requests.
const ONE_VM: &str = "/vmm/v4.3/ahv/config/vms/3d0c4a2e-1b8f-4c1a-9e2f-000000000001";
/// A second row that answers, on another path: what a liveness check has to have.
const ONE_HOST: &str = "/clustermgmt/v4.3/config/hosts/7b2f2f70-0f6a-4b58-9f9b-000000000020";

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

/// Five attempts, one presentation. The other four never reach the wire at all: the client
/// answers them itself, out of what the first one learned.
#[tokio::test]
async fn a_refused_credential_is_presented_exactly_once() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "wrong").unwrap();
    for _ in 0..5 {
        let e = c
            .list_page(vm(), 0, &ListOptions::default())
            .await
            .expect_err("a wrong password is refused");
        assert!(matches!(e, PrismError::Auth), "{e:?}");
    }
    assert_eq!(pc.requests().len(), 1, "one presentation, five attempts");
    assert!(c.auth_rejected());
}

/// Negotiation fans out across the namespaces. A wrong password must still cost exactly one
/// request. An unguarded fan-out puts four passwords on the wire before the first answer comes
/// back, and against a Prism Central that counts failures that is four, not one.
#[tokio::test]
async fn a_wrong_password_costs_one_request_even_though_negotiation_fans_out() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "wrong").unwrap();
    assert!(c.fan_out() > 1, "the burst this test is about");
    let e = c
        .negotiate()
        .await
        .expect_err("a wrong password is refused");
    assert!(matches!(e, PrismError::Auth), "{e:?}");
    assert_eq!(pc.requests().len(), 1);
}

/// The single-flight is only for the *first* authentication. Once one answer has proved the
/// credential good, concurrent requests run concurrently again: a gate held for the session
/// would turn the whole client into one request at a time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_run_concurrently_once_the_credential_is_established() {
    let gate = Gate::new();
    let pc = MockPc::builder().hold_path(VMS, &gate).start().await;
    let c = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    // One answered request on an unheld path, so the credential is established before the two
    // below go out. Without it the first of them would be the authentication, and it is held.
    let _ = c.get_path(OTHER).await;

    let a = tokio::spawn({
        let c = Arc::clone(&c);
        async move { c.list_page(vm(), 0, &ListOptions::default()).await }
    });
    let b = tokio::spawn({
        let c = Arc::clone(&c);
        async move { c.list_page(vm(), 0, &ListOptions::default()).await }
    });
    // Both are recorded before the gate, so a shut gate still counts them as arrived.
    for _ in 0..200 {
        if pc.requests_to(VMS).len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        pc.requests_to(VMS).len(),
        2,
        "the second request is queued behind an authentication that already happened"
    );
    gate.release(2);
    assert!(a.await.unwrap().is_ok());
    assert!(b.await.unwrap().is_ok());
}

/// One endpoint answers 401 on a session that everything else still honours. That is the
/// endpoint's verdict on the account, not a refused credential: the session is checked on a
/// URL it has already answered, and because it still answers there no password goes out, the
/// error names the denial, nothing latches, and the session goes on being ridden.
///
/// The case is real: a Prism Central whose IAM service answered 401 on `users` spent one
/// presentation per run for the whole of a day, and each run ended with the credential
/// "refused" while it was fine.
#[tokio::test]
async fn a_401_from_one_endpoint_on_a_live_session_spends_no_password() {
    let pc = MockPc::builder().fail_path(VMS, 401).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.get_path(ONE_HOST).await.unwrap();
    assert_eq!(presentations(&pc), 1);

    let e = c
        .list_page(vm(), 0, &ListOptions::default())
        .await
        .expect_err("401");
    assert!(matches!(e, PrismError::Denied(_)), "{e:?}");
    assert!(!c.auth_rejected(), "the credential was never refused");
    assert_eq!(presentations(&pc), 1, "and never presented again");
    // The check itself: the session, on the URL that had answered, and it answered again.
    assert_eq!(
        pc.requests_to(ONE_HOST).len(),
        2,
        "one answer, one liveness check"
    );

    c.get_path(ONE_HOST).await.unwrap();
    assert_eq!(
        presentations(&pc),
        1,
        "the session is still the one being ridden"
    );
}

/// The other half of the same coin. The session really has ended *and* the renewal lands on an
/// endpoint that refuses it: the liveness check finds the session dead, one password goes out,
/// its 401 is the credential being refused, and that latches - exactly as before.
#[tokio::test]
async fn a_refusal_after_a_real_expiry_still_latches() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.get_path(ONE_HOST).await.unwrap();
    c.get_path(ONE_VM).await.unwrap();
    assert_eq!(presentations(&pc), 1);

    pc.expire_session();
    pc.fail_from_now(ONE_VM, 401);
    let e = c.get_path(ONE_VM).await.expect_err("refused");
    assert!(matches!(e, PrismError::Auth), "{e:?}");
    assert!(c.auth_rejected());
    assert_eq!(
        presentations(&pc),
        2,
        "the one that opened the session and the one that was refused"
    );

    let before = pc.requests().len();
    for _ in 0..5 {
        let _ = c.get_path(ONE_HOST).await;
    }
    assert_eq!(
        pc.requests().len(),
        before,
        "nothing goes out after the credential is refused"
    );
}

/// How many of the recorded requests carried a credential rather than riding the session.
fn presentations(pc: &MockPc) -> usize {
    pc.requests().iter().filter(|r| r.authenticates()).count()
}

/// How many carried neither, and so could only ever have been answered 401.
fn bare(pc: &MockPc) -> usize {
    pc.requests()
        .iter()
        .filter(|r| !r.authenticates() && r.header("cookie").is_none())
        .count()
}

/// Every request carries something the far end can authenticate: the credential, or a session.
/// Never neither.
///
/// The structural invariant the rest of this file's arithmetic rests on. A request carrying
/// neither is answered 401 whatever the state of the world,
/// and that 401 is indistinguishable from a session ending - so it drops the session and buys a
/// renewal, and a second password goes out for a single expiry. Under concurrency that is a
/// race; here it is a sequence, and it holds against all three Prism Centrals that have
/// anything to say about sessions.
#[tokio::test]
async fn no_request_ever_carries_neither_a_credential_nor_a_session() {
    let cases = [
        ("a Prism Central that sets no cookie", None, false),
        ("a session that ends every other answer", Some(2), true),
        ("a cookie that is never accepted at all", Some(0), true),
    ];
    for (what, expire_after, cookies) in cases {
        let mut builder = MockPc::builder();
        if !cookies {
            builder = builder.no_session_cookies();
        }
        if let Some(n) = expire_after {
            builder = builder.expire_session_after(n);
        }
        let pc = builder.start().await;
        let c = Client::connect(&profile(&pc), "secret").unwrap();
        for _ in 0..8 {
            c.get_path(ONE_VM).await.unwrap();
        }
        assert_eq!(
            bare(&pc),
            0,
            "{what}: {} of {} requests could only ever have been refused",
            bare(&pc),
            pc.requests().len()
        );
    }
}

/// The measurement this whole change is for. A session that lists VMs and walks pages makes
/// dozens of requests; exactly one of them may carry the password.
///
/// Before the session cookie, every single one did - about two to five thousand presentations
/// an hour against a Prism Central that counts them.
#[tokio::test]
async fn startup_authenticates_exactly_once() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    c.negotiate().await.unwrap();
    let _ = c.list_page(vm(), 0, &ListOptions::default()).await;
    let _ = c.get_path(OTHER).await;

    assert!(
        pc.requests().len() >= 20,
        "only {} requests: the burst this measures is missing",
        pc.requests().len()
    );
    assert_eq!(presentations(&pc), 1, "one password for the whole startup");
}

/// A session does not last for ever. When it ends, one request renews it and the rest ride the
/// new one - the renewal is not per request.
#[tokio::test]
async fn an_expired_session_is_renewed_once_and_then_ridden_again() {
    // Two answers on the session, then it is dead.
    let pc = MockPc::builder().expire_session_after(2).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    for _ in 0..6 {
        let _ = c.get_path(OTHER).await;
    }
    assert_eq!(pc.requests().len(), 7, "six, plus the one that was refused");
    assert_eq!(
        presentations(&pc),
        2,
        "one to open the session and one to renew it"
    );
}

/// N requests holding the same stale cookie produce **one** re-authentication between them.
///
/// This is the asymmetry the design rests on: a 401 on a cookie-only request is expiry, and the
/// first request to notice it is the only one that presents a password. A 401 on a
/// password-bearing request is refusal, and nothing retries that at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn n_stale_requests_produce_one_re_authentication() {
    let pc = MockPc::builder().start().await;
    let c = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    c.get_path(ONE_VM).await.unwrap();
    assert_eq!(presentations(&pc), 1, "the session is open");
    // The session has to have worked before it ends, or a 401 on it means the cookie never
    // authenticated anything - a different rule, with a test of its own below.
    for _ in 0..2 {
        c.get_path(ONE_VM).await.unwrap();
    }
    assert_eq!(presentations(&pc), 1, "and it is being ridden");

    // A clear rate window, so all three of the requests below go out together rather than the
    // last of them being paced past the renewal it is supposed to be racing.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    pc.expire_session();
    // And the mock holds each answer back a third of a second, so all three are in flight -
    // and all three are refused - before any of them can renew.
    pc.set_delay(ONE_VM, Duration::from_millis(300));
    let spent = pc.requests().len();
    let stale: Vec<_> = (0..3)
        .map(|_| {
            let c = Arc::clone(&c);
            tokio::spawn(async move { c.get_path(ONE_VM).await })
        })
        .collect();
    for t in stale {
        assert!(t.await.unwrap().is_ok());
    }

    assert_eq!(
        pc.requests().len() - spent,
        6,
        "three refusals and three answers"
    );
    assert_eq!(
        presentations(&pc),
        2,
        "one to open the session, one to renew it, and none for the other two"
    );
}

/// A Prism Central whose cookie is never accepted must not double every request for ever. One
/// cookie refused far too soon after it was issued means cookies do not work here, and the
/// client goes back to presenting the credential on each request - today's behaviour, which is
/// safe, rather than twice today's, which is not.
#[tokio::test]
async fn a_cookie_that_never_works_is_abandoned_rather_than_retried_for_ever() {
    let pc = MockPc::builder().expire_session_after(0).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    for _ in 0..5 {
        let _ = c.get_path(OTHER).await;
    }
    assert_eq!(
        pc.requests().len(),
        7,
        "two refusals learn it; the rest carry the credential straight away"
    );
    assert!(!c.auth_rejected(), "a stale cookie is not a bad password");
}

/// The last resort. Every rule above is a rule about what the client *concludes*; this one is a
/// rule about what it is *able to do*, and it holds whether or not the conclusions are right.
///
/// A Prism Central that refuses every credential sees at most [`valve::PRESENTATIONS`] of them
/// in [`valve::WINDOW`], and then this client stops making requests at all - the state machine
/// stops it at the first, and the valve is what stops it if the state machine ever does not.
#[tokio::test]
async fn the_valve_latches_and_the_session_reports_itself_stopped() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "wrong").unwrap();
    for _ in 0..20 {
        let e = c.get_path(ONE_VM).await.expect_err("refused");
        assert!(matches!(e, PrismError::Auth), "{e:?}");
    }
    assert!(
        presentations(&pc) <= valve::PRESENTATIONS,
        "{} presentations against a cap of {}",
        presentations(&pc),
        valve::PRESENTATIONS
    );
    assert!(c.auth_rejected(), "the session says it has stopped");

    // And it is latched: nothing at all goes out afterwards, whatever is asked for.
    let spent = pc.requests().len();
    let _ = c.list_page(vm(), 0, &ListOptions::default()).await;
    let _ = c.negotiate().await;
    assert_eq!(pc.requests().len(), spent);
}

/// Eight requests, all holding the one session, and the session dies under all eight at once:
/// **two** presentations in total, the one that opened it and the one that renewed it.
///
/// An exact number rather than a ceiling, and it is exact because the test says when each thing
/// happens rather than hoping the timing obliges. The gate parks all eight at the Prism Central
/// before it authenticates any of them, so `expire_session` lands while every one of them is in
/// flight holding the same cookie. That is the worst case the design has to survive - the whole
/// fleet stale at once - and it has one answer, not a range.
///
/// This replaces a stress test that fired sixty-four requests through a session that died every
/// tenth answer and asserted a bound. It failed about one run in three, and it was right to:
/// the client was letting requests out that carried neither the session nor the credential,
/// whose certain 401 read as one more session ending and bought a second renewal for a single
/// expiry. A bound over a race would have hidden that; an exact count over a sequence cannot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_never_renew_a_session_twice() {
    const STALE: usize = 8;

    let gate = Gate::new();
    let pc = MockPc::builder().hold_path(ONE_VM, &gate).start().await;
    let c = Arc::new(Client::connect(&profile(&pc), "secret").unwrap());
    // The session, opened and then ridden once, both on a path the gate does not hold. What
    // that path answers is beside the point - a Prism Central authenticates before it routes,
    // so a 404 opens a session just as well - but the ride is not: a cookie that has never
    // authenticated anything is abandoned rather than renewed, which is a different rule with
    // a test of its own above.
    let _ = c.get_path(OTHER).await;
    let _ = c.get_path(OTHER).await;
    assert_eq!(presentations(&pc), 1, "the session is open and working");

    let stale: Vec<_> = (0..STALE)
        .map(|_| {
            let c = Arc::clone(&c);
            tokio::spawn(async move { c.get_path(ONE_VM).await })
        })
        .collect();
    // All eight have reached the Prism Central and are waiting at the gate. They are recorded
    // before it, so this counts the ones still held.
    for _ in 0..600 {
        if pc.requests_to(ONE_VM).len() == STALE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        pc.requests_to(ONE_VM).len(),
        STALE,
        "all eight are in flight"
    );

    // The session dies under all eight, and then they are let go together. Every one of them is
    // answered 401, every one of them comes back round, and between them they renew once.
    pc.expire_session();
    // Enough for the eight refusals, the renewal, and the eight answers after it. Spare permits
    // sit in the gate unspent, so there is no count to get wrong here.
    gate.release(STALE * 3);
    for t in stale {
        t.await
            .unwrap()
            .expect("the renewal answers every one of them");
    }

    assert_eq!(
        presentations(&pc),
        2,
        "one to open the session and one to renew it, for eight stale requests"
    );
    assert_eq!(
        bare(&pc),
        0,
        "and nothing went out that could only be refused"
    );
    assert!(!c.auth_rejected());
}

/// A Prism Central that sets no cookie has nothing to reuse, and this client degrades to what
/// it did before session reuse existed: the credential on every request, and no gate.
///
/// What it must *not* do is spend anything rediscovering that. Two presentations that open no
/// session settle it, and they are two of the answers the caller asked for rather than two
/// refusals bought to find out: six calls, six requests, not one wasted round trip.
#[tokio::test]
async fn a_prism_central_that_sets_no_cookie_is_not_asked_twice_for_ever() {
    let pc = MockPc::builder().no_session_cookies().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    for _ in 0..6 {
        c.get_path(ONE_VM).await.unwrap();
    }
    assert_eq!(pc.requests().len(), 6, "six answered, and nothing else");
    assert!(!c.auth_rejected());
    // And from here it is exactly the old behaviour: one request, one credential.
    let spent = pc.requests().len();
    c.get_path(ONE_VM).await.unwrap();
    assert_eq!(pc.requests().len() - spent, 1);
    assert_eq!(presentations(&pc), pc.requests().len());
}

/// The ring `:activity` reads keeps a refused request as a row of its own: a poller that has
/// gone quiet must not be the one invisible stall on the screen, any more than in the log.
#[tokio::test]
async fn a_refused_credential_is_in_the_ring_as_refused() {
    let pc = MockPc::builder().start().await;
    let c = Client::connect(&profile(&pc), "wrong").unwrap();
    for _ in 0..3 {
        let _ = c.list_page(vm(), 0, &ListOptions::default()).await;
    }
    let calls = c.metrics().calls();
    assert_eq!(calls.len(), 3, "one row per attempt: {calls:?}");
    let refused: Vec<_> = calls.iter().filter(|c| c.credential == "refused").collect();
    assert_eq!(refused.len(), 2, "two never went out");
    assert!(
        refused
            .iter()
            .all(|c| c.status.is_none() && c.method == "-"),
        "no status and no method for a request that never went out: {refused:?}"
    );
    let presented = calls.last().unwrap();
    assert_eq!(
        (presented.credential, presented.status),
        ("presented", Some(401))
    );
    assert!(presented.path.starts_with("/api/"), "{}", presented.path);
    assert!(!presented.path.contains("127.0.0.1"), "path, not URL");
}
