//! The pacing the Prism Central itself asks for.
//!
//! A pc.7.6 Prism Central puts these on **every** answer, a plain 200 included:
//!
//! ```text
//! x-api-ratelimit-limit: 3        x-api-ratelimit-refresh-period-seconds: 1
//! x-api-ratelimit-remaining: 2
//! x-ratelimit-limit: 80           x-ratelimit-remaining: 79     x-ratelimit-reset: 0
//! ```
//!
//! Three a second. Pacing at thirty is ten times the limit, and a sustained ten-fold overrun
//! against a gateway provokes 401s, intermittent 503s and account lockouts.

use std::time::{Duration, Instant};

use nutsh_catalog::{Kind, RateLimit, kind};
use nutsh_mockpc::{GENEROUS, MockPc, RecordedRequest};
use nutsh_prism::bucket::{HOST_CEILING, HOST_START};
use nutsh_prism::{Client, ListOptions, Profile};

/// A path with no fixture behind it: the answer is a 404, which costs the Prism Central exactly
/// as much as a 200 and is therefore just as much a request the pacing has to cover.
const ANY_PATH: &str = "/vmm/v4.3/ahv/config/vms/00000000-0000-4000-8000-000000000000";

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

/// A tenth of a second off the window the requests are counted in.
///
/// The client paces itself when it *takes* a token; the mock stamps a request when its handler
/// is *scheduled*, which is after the connect on the first request and after the runtime gets
/// round to it on every one. That spread is the harness, not the pacing, and measuring a
/// saturated sliding-window limiter against a window of exactly its own period puts every
/// assertion on the boundary. Nine hundred milliseconds absorbs the spread and still convicts
/// anything running faster than the tier: an unpaced client puts all seven requests inside a
/// single millisecond.
const SLACK: Duration = Duration::from_millis(100);

/// The most requests the mock received inside any `window`. A rate, measured where the requests
/// actually landed: the client's own meter is a ten-second average and would report `0.7 req/s`
/// for seven requests fired inside one second.
fn peak_within(requests: &[RecordedRequest], window: Duration) -> usize {
    requests
        .iter()
        .map(|r| {
            let end = r.at + window;
            requests
                .iter()
                .filter(|o| o.at >= r.at && o.at < end)
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Nothing has been advertised yet, so nothing may be assumed. Three a second is what the recording's
/// Prism Central turned out to allow; starting at thirty and waiting to be told otherwise is
/// starting ten times over a limit the server never had to state.
#[test]
fn the_ceiling_starts_at_what_the_tightest_known_prism_central_allows() {
    assert_eq!(
        HOST_START,
        RateLimit {
            count: 3,
            per_secs: 1
        }
    );
    assert!(
        !HOST_CEILING.tighter_than(HOST_START),
        "the absolute cap may not be tighter than the start"
    );
}

/// The mock's default tier and the client's absolute cap are the same number on purpose: the
/// default mock never paces anybody, so no test that is not about pacing pays a second for it.
#[test]
fn the_default_mock_advertises_the_clients_absolute_cap() {
    assert_eq!(GENEROUS, HOST_CEILING);
}

/// The headers are advisory - they appear in none of the ninety-three spec files - so a Prism
/// Central that sends none must leave the conservative start standing rather than unthrottle us.
#[tokio::test]
async fn a_prism_central_that_advertises_nothing_keeps_the_conservative_start() {
    let pc = MockPc::builder().no_rate_limit_headers().start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let _ = c.get_path(ANY_PATH).await;
    assert_eq!(c.host_limit(), HOST_START);
}

/// A tier the server states is adopted in both directions: wider than the start when it is
/// generous, and tighter than the start when it is not.
#[tokio::test]
async fn the_advertised_tier_replaces_the_assumption() {
    let wide = MockPc::builder().rate_limit(10, 1).start().await;
    let c = Client::connect(&profile(&wide), "secret").unwrap();
    let _ = c.get_path(ANY_PATH).await;
    assert_eq!(
        c.host_limit(),
        RateLimit {
            count: 10,
            per_secs: 1
        }
    );

    let narrow = MockPc::builder().rate_limit(1, 2).start().await;
    let c = Client::connect(&profile(&narrow), "secret").unwrap();
    let _ = c.get_path(ANY_PATH).await;
    assert_eq!(
        c.host_limit(),
        RateLimit {
            count: 1,
            per_secs: 2
        }
    );
}

/// However large a number the far end states, this client will not go past the ceiling it was
/// already willing to run at. A header is a string from the network: it may widen the budget up
/// to a bound, never past one.
#[tokio::test]
async fn an_extravagant_advertisement_is_capped() {
    let pc = MockPc::builder().rate_limit(100_000, 1).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let _ = c.get_path(ANY_PATH).await;
    assert_eq!(c.host_limit(), HOST_CEILING);
}

/// The property the release turns on. Seven requests against a Prism Central that allows three
/// a second may not put four into any second, and cannot be finished inside two.
///
/// Against the old hard-coded thirty a second all seven went out inside one millisecond.
#[tokio::test]
async fn we_never_exceed_the_rate_the_prism_central_advertises() {
    let pc = MockPc::builder().rate_limit(3, 1).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let started = Instant::now();
    for _ in 0..7 {
        let _ = c.get_path(ANY_PATH).await;
    }
    let elapsed = started.elapsed();
    let requests = pc.requests();
    let window = Duration::from_secs(1) - SLACK;
    let peak = peak_within(&requests, window);
    assert_eq!(requests.len(), 7);
    assert!(
        peak <= 3,
        "{peak} requests inside {window:?} against a limit of three a second"
    );
    assert!(
        elapsed >= Duration::from_millis(1900),
        "seven requests at three a second cannot finish in {elapsed:?}"
    );
}

/// The longer budget is honoured too. Its window length is stated nowhere, so nothing is
/// guessed about it: when `x-ratelimit-remaining` reaches zero the client holds for
/// `x-ratelimit-reset` instead of spending the request that would have earned the 429.
#[tokio::test]
async fn an_exhausted_budget_stops_us_before_the_server_has_to() {
    let pc = MockPc::builder().rate_budget(2, 1).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let started = Instant::now();
    for _ in 0..3 {
        let _ = c.get_path(ANY_PATH).await;
    }
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "the third request went out on an exhausted budget"
    );
}

/// The startup burst is the worst moment: twenty-odd probes fired at once against a limit of
/// three a second. The fan-out may never be wider than the ceiling allows.
#[tokio::test]
async fn the_negotiation_fan_out_never_exceeds_the_ceiling() {
    let pc = MockPc::builder().rate_limit(3, 1).start().await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    // One request first, so the tier is learned before the burst rather than during it.
    let _ = c.list_page(vm(), 0, &ListOptions::default()).await;
    assert!(
        c.fan_out() <= 3,
        "fan-out {} over a limit of 3",
        c.fan_out()
    );
}

/// A reset this Prism Central will not stand behind is clamped, not believed.
///
/// `x-ratelimit-remaining: 0` with `x-ratelimit-reset: 9223372036854775807` - an epoch stamp
/// where a delta was meant - made the hold `now + 292 billion years`, and `Instant + Duration`
/// panics on overflow. It landed on the first negotiation probe, so `--check`, `--snapshot` and
/// the TUI all died before a frame was drawn.
#[tokio::test]
async fn a_reset_that_cannot_be_true_does_not_bring_the_client_down() {
    // A budget of one, spent by the request that reads it: the hold is armed on the first
    // answer, rather than only on a second.
    let pc = MockPc::builder()
        .rate_budget(1, 9_223_372_036_854_775_807)
        .start()
        .await;
    let c = Client::connect(&profile(&pc), "secret").unwrap();
    let _ = c.get_path(ANY_PATH).await;
    assert_eq!(
        pc.requests().len(),
        1,
        "the request was answered, not aborted"
    );
}

/// A tier is stated **per endpoint**, and the host bucket holds one number.
///
/// A single Prism Central states ten different tiers across its endpoints - 2, 3, 5, 6, 7, 8,
/// 10, 15, 20, 25 and 30 a second - and a host bucket holding one number takes whichever answer
/// spoke last. So a `30/s` endpoint's answer widens the bucket that a `3/s` endpoint's
/// requests then pass through, and the Disaster Recovery page drove
/// `dataprotection/v4.4/config/protected-resources` to four a second against the three it had
/// advertised. The host ceiling is not the thing at fault: nothing was holding an endpoint to
/// what that endpoint itself said.
///
/// The catalog's rate stays the declared ceiling. A header may lower it and may not raise it:
/// widening from the wire is how this client came to run ten times over the limit for a whole
/// session, which is what the account lockouts are laid at.
#[tokio::test]
async fn an_endpoint_is_paced_by_the_tier_that_endpoint_advertised() {
    let vms = vm();
    assert_eq!(
        vms.rate,
        RateLimit {
            count: 2,
            per_secs: 1
        },
        "the catalog's declared rate for this operation, which the rest of this test is about"
    );

    // Tighter than the catalog: adopted, so the operation slows to what its own answers said.
    let tight = MockPc::builder().rate_limit(1, 1).start().await;
    let c = Client::connect(&profile(&tight), "secret").unwrap();
    let _ = c.list_page(vms, 0, &ListOptions::default()).await;
    assert_eq!(
        c.bucket_limit("GET", vms.list_path),
        Some(RateLimit {
            count: 1,
            per_secs: 1
        }),
        "the endpoint said one a second and the catalog said two"
    );

    // Wider than the catalog: refused. The wire may not raise this client's own ceiling.
    let wide = MockPc::builder().rate_limit(25, 1).start().await;
    let c = Client::connect(&profile(&wide), "secret").unwrap();
    let _ = c.list_page(vms, 0, &ListOptions::default()).await;
    assert_eq!(
        c.bucket_limit("GET", vms.list_path),
        Some(vms.rate),
        "a generous endpoint does not widen the operation past what the catalog declares"
    );
    // ...while the host bucket does take it, which is the asymmetry this test exists to pin.
    assert_eq!(
        c.host_limit(),
        RateLimit {
            count: 25,
            per_secs: 1
        }
    );
}
