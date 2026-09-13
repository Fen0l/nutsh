//! The request meter: what `2.4 req/s · 68% cached` is computed from.
//!
//! One `Arc<Metrics>` is owned by [`crate::Client`] and reachable through `Client::metrics()`.
//! Every counter is written from inside `send`, `acquire` and `drain_bucket` - the three
//! funnels every request, every pacing sleep and every 429 pass through - so the meter counts
//! what went out rather than what a caller meant to send. The clock is an argument, never
//! `Instant::now()` inside a method, which is what makes the whole file testable against a
//! synthetic clock; `Bucket::take(now)` set that precedent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// One-second buckets in the ring: the ratio's window, and the whole of the ring.
pub const BUCKETS: u64 = 60;
/// Buckets the rate averages over. Ten seconds reacts to a burst within a couple of frames;
/// sixty would smear it into invisibility.
pub const RATE_BUCKETS: u64 = 10;
/// The largest rate [`Metrics::snapshot`] will report, which is what keeps the field
/// `meter_text` prints four characters wide. The clamp is `snapshot`'s alone: a [`Meter`] built
/// by hand - which the tests and the TUI's snapshot fixtures do - prints what it was given.
const MAX_RATE: f64 = 99.9;

/// One second's counts. `second` is what makes the ring lazy: a slot whose second is not the
/// one being written is reset rather than added to, and a slot outside the window being summed
/// is skipped, so there is no advance step and no clearing pass.
#[derive(Debug, Clone, Copy)]
struct Slot {
    /// Seconds since the metrics' origin, or `u64::MAX` for a slot never written.
    second: u64,
    /// Requests that went out. The rate's input, and not the ratio's.
    made: u64,
    /// Requests a cache spared us; see [`Metrics::record_avoided`].
    avoided: u64,
    /// Requests that went out and a cache could have spared; see
    /// [`Metrics::record_avoidable`].
    avoidable_made: u64,
    /// `acquire` slept, or a 429 drained a bucket.
    throttled: u64,
    /// The pipe failed: connect, transport, TLS, a certificate, 401, or a 5xx.
    failed: u64,
}

const EMPTY: Slot = Slot {
    second: u64::MAX,
    made: 0,
    avoided: 0,
    avoidable_made: 0,
    throttled: 0,
    failed: 0,
};

#[derive(Debug)]
struct Ring {
    slots: [Slot; BUCKETS as usize],
}

impl Ring {
    fn new() -> Ring {
        Ring {
            slots: [EMPTY; BUCKETS as usize],
        }
    }

    /// The slot for `second`, reset first when it still holds an older one.
    fn at(&mut self, second: u64) -> &mut Slot {
        let i = (second % BUCKETS) as usize;
        if self.slots[i].second != second {
            self.slots[i] = Slot { second, ..EMPTY };
        }
        &mut self.slots[i]
    }

    /// The sum of the `window` seconds ending at `now`, inclusive.
    fn sum(&self, now: u64, window: u64) -> Totals {
        let lo = now.saturating_sub(window.saturating_sub(1));
        let mut total = Totals::default();
        for s in &self.slots {
            if s.second >= lo && s.second <= now {
                total.made += s.made;
                total.avoided += s.avoided;
                total.avoidable_made += s.avoidable_made;
                total.throttled += s.throttled;
                total.failed += s.failed;
            }
        }
        total
    }
}

/// One window's counts, summed out of the ring by [`Ring::sum`]. A type of its own rather than
/// a [`Slot`], so nothing in it names a second: a total spans a window and belongs to no one
/// second in it.
#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    made: u64,
    avoided: u64,
    avoidable_made: u64,
    throttled: u64,
    failed: u64,
}

/// What the status line draws, computed once per frame from [`Metrics::snapshot`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    /// Requests a second over the last ten buckets, clamped at 99.9.
    pub rate: f64,
    /// `avoided / (avoided + avoidable_made)` over the last sixty buckets, or `None` when
    /// nothing avoidable happened at all - which is the steady state of a single-page table,
    /// not an edge case. `None` prints `-`, never `0%`: printing `0%` would report the cache
    /// as failing at the moment it has nothing to do.
    pub cached: Option<f64>,
    /// The local rate budget paced us in the last ten seconds.
    pub throttled: bool,
    /// The pipe failed in the last ten seconds. Beats `throttled` at the drawing end.
    pub failed: bool,
}

#[derive(Debug)]
pub struct Metrics {
    origin: Instant,
    started: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    throttled: AtomicU64,
    rate_limited: AtomicU64,
    elapsed_micros: AtomicU64,
    /// The precedent is `Client::buckets`: a `Mutex` held for nanoseconds on a path that is
    /// about to await network I/O. Never held across an await.
    ring: Mutex<Ring>,
}

impl Default for Metrics {
    fn default() -> Metrics {
        Metrics::since(Instant::now())
    }
}

impl Metrics {
    /// A metrics whose ring is anchored at `origin`. Public to the crate only: a caller
    /// outside has no reason to choose the origin, and a test inside chooses it to have a
    /// clock it can name seconds on.
    pub(crate) fn since(origin: Instant) -> Metrics {
        Metrics {
            origin,
            started: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            throttled: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            elapsed_micros: AtomicU64::new(0),
            ring: Mutex::new(Ring::new()),
        }
    }

    /// Whole seconds from the origin. A `now` before it - which cannot happen with a monotone
    /// clock, and is cheap to be safe about - is second zero.
    fn second(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.origin).as_secs()
    }

    /// A poisoned meter is taken as it stands: the worst a panic under the guard can leave is
    /// one second's counts, and panicking a whole session's requests over it would be worse.
    fn ring(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A request is about to go on the wire. The retry after a 429 calls this a second time:
    /// it cost the Prism Central twice, so it counts twice.
    pub(crate) fn record_started(&self, now: Instant) {
        self.started.fetch_add(1, Ordering::Relaxed);
        let s = self.second(now);
        self.ring().at(s).made += 1;
    }

    /// A response arrived, whatever it said. A 404 and a 403 are *results*: they land here.
    pub(crate) fn record_completed(&self, elapsed: Duration) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        self.elapsed_micros.fetch_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// The pipe failed, not the payload: connect, transport, TLS, a certificate, a 401, a 5xx.
    pub(crate) fn record_failed(&self, now: Instant) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        let s = self.second(now);
        self.ring().at(s).failed += 1;
    }

    /// `acquire` slept on the local budget. The first time the program has ever shown that it
    /// is pacing itself; today it sleeps and logs.
    pub(crate) fn record_throttled(&self, now: Instant) {
        self.throttled.fetch_add(1, Ordering::Relaxed);
        let s = self.second(now);
        self.ring().at(s).throttled += 1;
    }

    /// A 429 drained a bucket. Its own lifetime counter, and the same bucket as `throttled`:
    /// the amber says "the budget is holding you up", and a served 429 is the sharpest form of
    /// that.
    pub(crate) fn record_rate_limited(&self, now: Instant) {
        self.rate_limited.fetch_add(1, Ordering::Relaxed);
        let s = self.second(now);
        self.ring().at(s).throttled += 1;
    }

    /// `n` requests a cache spared: the namespaces pinned from disk instead of negotiated, the
    /// cluster resolved from disk, the pages an armed probe's skip did not walk.
    pub fn record_avoided(&self, now: Instant, n: u64) {
        let s = self.second(now);
        self.ring().at(s).avoided += n;
    }

    /// `n` requests that went out and a cache could have spared: the requests negotiation took,
    /// the cluster resolution, the probe and the pages of a probe-eligible walk. Counted
    /// *beside* `made`, never instead of it.
    pub fn record_avoidable(&self, now: Instant, n: u64) {
        let s = self.second(now);
        self.ring().at(s).avoidable_made += n;
    }

    /// Requests started since the client was built. The honest way for a test to assert that
    /// zero negotiation requests went out.
    pub fn started(&self) -> u64 {
        self.started.load(Ordering::Relaxed)
    }

    pub fn completed(&self) -> u64 {
        self.completed.load(Ordering::Relaxed)
    }

    pub fn rate_limited(&self) -> u64 {
        self.rate_limited.load(Ordering::Relaxed)
    }

    /// The mean round trip of the completed requests, or `None` before the first one. The two
    /// loads are not a consistent pair: a completion landing between them divides N+1 requests'
    /// micros by N, which is below the precision anyone reads a mean latency at.
    pub fn mean_latency(&self) -> Option<Duration> {
        let n = self.completed();
        (n > 0).then(|| Duration::from_micros(self.elapsed_micros.load(Ordering::Relaxed) / n))
    }

    /// One ring, two windows: the rate over ten buckets, the ratio over sixty.
    pub fn snapshot(&self, now: Instant) -> Meter {
        let s = self.second(now);
        let ring = self.ring();
        let short = ring.sum(s, RATE_BUCKETS);
        let long = ring.sum(s, BUCKETS);
        drop(ring);
        let denominator = long.avoided + long.avoidable_made;
        Meter {
            rate: ((short.made as f64) / (RATE_BUCKETS as f64)).min(MAX_RATE),
            cached: (denominator > 0).then(|| long.avoided as f64 / denominator as f64),
            throttled: short.throttled > 0,
            failed: short.failed > 0,
        }
    }
}

/// The meter as text, or `None` for a frame with no room for it. It degrades by losing the
/// least useful half first: the ratio is a property of the session and can be inferred from a
/// wider window, while the rate is the number a person watches move.
///
/// `frame_width` is the whole frame's width, not the meter chunk's - the thresholds below are
/// terminal sizes. The status line hands the meter a fixed chunk; passing that chunk's width
/// here would return `None` on every terminal there is.
pub fn meter_text(meter: &Meter, frame_width: u16) -> Option<String> {
    let rate = format!("{:.1} req/s", meter.rate);
    match frame_width {
        0..=79 => None,
        80..=99 => Some(rate),
        _ => {
            let cached = match meter.cached {
                Some(ratio) => format!("{}%", (ratio * 100.0).round() as u64),
                None => "-".to_string(),
            };
            Some(format!("{rate} · {cached} cached"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A metrics with a fixed origin, so every `now` below is an exact number of seconds.
    fn metrics() -> (Metrics, Instant) {
        let origin = Instant::now();
        (Metrics::since(origin), origin)
    }

    fn at(origin: Instant, secs: u64) -> Instant {
        origin + Duration::from_secs(secs)
    }

    /// The rate is `made` over the last ten buckets divided by ten: ten seconds reacts to a
    /// burst within a couple of frames, where sixty would smear it.
    #[test]
    fn the_rate_is_the_last_ten_seconds() {
        let (m, origin) = metrics();
        for _ in 0..24 {
            m.record_started(at(origin, 0));
        }
        assert_eq!(m.snapshot(at(origin, 9)).rate, 2.4, "24 in the window");
        assert_eq!(m.started(), 24, "and the lifetime counter agrees");
        // The tenth second past it has fallen out of the window.
        assert_eq!(m.snapshot(at(origin, 10)).rate, 0.0);
    }

    /// The ratio is over the last sixty buckets, and its denominator is `avoidable` work only.
    #[test]
    fn the_ratio_is_avoided_over_avoided_plus_avoidable() {
        let (m, origin) = metrics();
        m.record_avoided(at(origin, 0), 68);
        m.record_avoidable(at(origin, 30), 32);
        assert_eq!(m.snapshot(at(origin, 59)).cached, Some(0.68));
        // Sixty-one seconds on, the first bucket is gone and only the avoidable half is left.
        assert_eq!(m.snapshot(at(origin, 61)).cached, Some(0.0));
    }

    /// The single-page VM session, and the assertion that keeps the meter honest: a hundred
    /// requests with nothing avoidable among them reads `- cached`, never `0%`.
    #[test]
    fn a_session_with_nothing_avoidable_reads_an_em_dash() {
        let (m, origin) = metrics();
        for _ in 0..100 {
            m.record_started(at(origin, 3));
        }
        let meter = m.snapshot(at(origin, 3));
        assert_eq!(meter.cached, None, "no denominator, not a zero numerator");
        assert_eq!(meter_text(&meter, 120).unwrap(), "10.0 req/s · - cached");
    }

    /// Nothing has happened at all: a session with a client and no request yet.
    #[test]
    fn an_empty_window_is_zero_and_an_em_dash() {
        let (m, origin) = metrics();
        let meter = m.snapshot(at(origin, 0));
        assert_eq!((meter.rate, meter.cached), (0.0, None));
        assert!(!meter.throttled && !meter.failed);
        assert_eq!(meter_text(&meter, 120).unwrap(), "0.0 req/s · - cached");
    }

    /// Amber when the local budget paced us; red when the pipe broke; red beats amber. A 404
    /// is a *result*: it counts as completed and colours nothing.
    #[test]
    fn throttled_is_amber_failed_is_red_and_a_404_is_neither() {
        let (m, origin) = metrics();
        m.record_started(at(origin, 0));
        m.record_completed(Duration::from_millis(12));
        let meter = m.snapshot(at(origin, 0));
        assert!(
            !meter.throttled && !meter.failed,
            "a 404 is a completed request"
        );

        m.record_throttled(at(origin, 1));
        assert!(m.snapshot(at(origin, 1)).throttled);
        assert!(!m.snapshot(at(origin, 1)).failed);
        m.record_failed(at(origin, 2));
        let meter = m.snapshot(at(origin, 2));
        assert!(meter.failed, "and red beats amber at the drawing end");
        assert!(meter.throttled);
        // Both fall out of the ten-second window together.
        let quiet = m.snapshot(at(origin, 12));
        assert!(!quiet.throttled && !quiet.failed);
    }

    /// A 429 that reached `drain_bucket` counts as pacing too: the amber says "the budget is
    /// what is holding you up", and a served 429 is the sharpest form of that.
    #[test]
    fn a_rate_limit_is_amber_as_well() {
        let (m, origin) = metrics();
        m.record_rate_limited(at(origin, 0));
        assert!(m.snapshot(at(origin, 0)).throttled);
        assert_eq!(m.rate_limited(), 1);
    }

    /// A gap longer than the ring is the ring: every slot names its own second, so a stale one
    /// can never be summed into a window it does not belong to.
    #[test]
    fn a_gap_longer_than_the_ring_clears_it() {
        let (m, origin) = metrics();
        for _ in 0..50 {
            m.record_started(at(origin, 0));
        }
        assert_eq!(m.snapshot(at(origin, 0)).rate, 5.0);
        assert_eq!(m.snapshot(at(origin, 600)).rate, 0.0);
        // And a slot that wraps onto an old one is reset rather than added to.
        m.record_started(at(origin, 600));
        assert_eq!(m.snapshot(at(origin, 600)).rate, 0.1);
    }

    /// The field cannot grow, so the rate is clamped where the format stops.
    #[test]
    fn the_rate_is_clamped_at_the_width_of_its_field() {
        let (m, origin) = metrics();
        for _ in 0..5000 {
            m.record_started(at(origin, 0));
        }
        assert_eq!(m.snapshot(at(origin, 0)).rate, 99.9);
        assert_eq!(
            meter_text(&m.snapshot(at(origin, 0)), 120).unwrap(),
            "99.9 req/s · - cached"
        );
    }

    /// Three widths, and the half that goes first is the half a person can infer.
    #[test]
    fn the_text_degrades_by_width() {
        let meter = Meter {
            rate: 2.4,
            cached: Some(0.68),
            throttled: false,
            failed: false,
        };
        assert_eq!(meter_text(&meter, 120).unwrap(), "2.4 req/s · 68% cached");
        assert_eq!(meter_text(&meter, 100).unwrap(), "2.4 req/s · 68% cached");
        assert_eq!(meter_text(&meter, 99).unwrap(), "2.4 req/s");
        assert_eq!(meter_text(&meter, 80).unwrap(), "2.4 req/s");
        assert_eq!(meter_text(&meter, 79), None);
        // The percentage is rounded, not truncated, so 2/3 reads 67% and not 66%.
        let two_thirds = Meter {
            cached: Some(2.0 / 3.0),
            ..meter
        };
        assert_eq!(
            meter_text(&two_thirds, 120).unwrap(),
            "2.4 req/s · 67% cached"
        );
    }
}
