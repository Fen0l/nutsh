//! A preventive rate budget per operation, plus one host-wide ceiling.
//!
//! A pure state machine: `take` says whether a token was available and, when it was not, how
//! long to wait. The async half lives in `Client`, which sleeps that long and tries again - so
//! the pacing is testable against a synthetic clock and never against a real one.
//!
//! What it implements is a **sliding** window: a take is remembered for `per_secs`, and the
//! `count + 1`th take inside any such span waits for the oldest of them to fall out.
//!
//! Not a fixed window. One opened at the first take and spendable whole lets up to twice a tier
//! go out across a seam - five requests at t=0.99s and five more at t=1.01s for a `5 per 1s`
//! tier. A Prism Central can advertise as little as **three** a second, and doubling three is
//! the difference between pacing a gateway and hammering one: the 429s a seam relies on being
//! absorbed are, on a Prism Central with a lockout policy, exactly what must never be provoked.
//! A sliding window costs one `VecDeque` of at most `count` instants per bucket and can never
//! exceed the tier over any span.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use nutsh_catalog::RateLimit;

/// The most this client will ever allow itself against one Prism Central, whatever a response
/// advertises: a header is a string off the network, and a number in it may widen the budget up
/// to a bound and never past one.
///
/// It deliberately caps a per-path tier that is higher than it. One tier in the catalog is:
/// `prism.operations.Batch` at `40 per 1s`, which this paces at thirty a second, so that kind
/// can never reach its declared budget. Every other tier is well under the ceiling.
pub const HOST_CEILING: RateLimit = RateLimit {
    count: 30,
    per_secs: 1,
};

/// What this client allows itself **before any Prism Central has said otherwise**, and the one
/// number in this file that is not a guess: a pc.7.6 Prism Central advertises
/// `x-api-ratelimit-limit: 3` over a one-second refresh period on every answer it gives.
///
/// Not [`HOST_CEILING`], which is ten times it. Running ten times over a gateway's limit for a
/// whole session provokes 401s, intermittent 503s and account lockouts, so the assumption starts
/// at the tightest limit any Prism Central has been observed to state, and only
/// [`Bucket::relimit`] from a header the server sent may widen it.
pub const HOST_START: RateLimit = RateLimit {
    count: 3,
    per_secs: 1,
};

#[derive(Debug)]
pub struct Bucket {
    limit: RateLimit,
    /// When each remembered take happened, oldest first: pushed in `Instant` order by `take`,
    /// so it is sorted, and pruned from the front once an entry is a whole window old. At most
    /// `limit.count` entries survive a prune, because `take` refuses past that.
    taken: VecDeque<Instant>,
    /// A 429 holds the bucket empty until this instant, whatever the window says.
    blocked_until: Option<Instant>,
}

impl Bucket {
    pub fn new(limit: RateLimit) -> Bucket {
        let mut b = Bucket {
            limit,
            taken: VecDeque::new(),
            blocked_until: None,
        };
        b.relimit(limit);
        b
    }

    /// The wait `take` would report, spending nothing. `Client::acquire` peeks the host ceiling
    /// and the operation's budget together and only takes when both say yes, so a caller parked
    /// on a tight tier does not burn a host token per attempt it never uses.
    pub fn peek(&self, now: Instant) -> Option<Duration> {
        if let Some(until) = self.blocked_until {
            // Past the hold the window is cleared by `take`, so the next token is free.
            return (now < until).then(|| until - now);
        }
        let per = self.per();
        let count = self.limit.count as usize;
        let live = self.live(now).count();
        if live < count {
            return None;
        }
        // `live - count` more of the oldest live takes have to fall out before there is room,
        // which is one more than the budget is over by. With `live == count` that is the
        // oldest take, and the wait is the rest of its window.
        let freed = self.live(now).nth(live - count)?;
        Some(per - now.duration_since(*freed))
    }

    /// Take a token, or say how long to wait for one.
    pub fn take(&mut self, now: Instant) -> Option<Duration> {
        if let Some(wait) = self.peek(now) {
            return Some(wait);
        }
        if self.blocked_until.take().is_some() {
            self.taken.clear();
        }
        let per = self.per();
        while self
            .taken
            .front()
            .is_some_and(|t| now.duration_since(*t) >= per)
        {
            self.taken.pop_front();
        }
        self.taken.push_back(now);
        None
    }

    /// A 429 arrived: empty the bucket and hold it empty for `retry_after`.
    ///
    /// The later deadline wins. Two operations sharing the host ceiling can each be told to
    /// wait, and a `Retry-After: 1` on one must not cut short the minute the server asked for
    /// on the other.
    pub fn drain(&mut self, now: Instant, retry_after: Duration) {
        self.taken.clear();
        for _ in 0..self.limit.count {
            self.taken.push_back(now);
        }
        // `Instant + Duration` panics on overflow and `retry_after` came off the network.
        // Both callers clamp it to `MAX_RETRY_AFTER` before it gets here; this is the floor
        // under them, and it falls back to the same number rather than to no hold at all.
        let until = now
            .checked_add(retry_after)
            .unwrap_or_else(|| now + crate::limits::MAX_RETRY_AFTER);
        self.blocked_until = Some(self.blocked_until.map_or(until, |held| held.max(until)));
    }

    /// The debt in the current window: the takes still inside it, which a 429 hold saturates to
    /// the tier's ceiling. What a test asserts on.
    pub fn debt(&self) -> u32 {
        u32::try_from(self.taken.len()).unwrap_or(u32::MAX)
    }

    /// The tier in force.
    pub fn limit(&self) -> RateLimit {
        self.limit
    }

    /// Adopt the tier a Prism Central advertised. `count` and `per_secs` are clamped to at
    /// least one, which is also what [`Bucket::new`] applies, so a `0` off the network is a
    /// budget of one rather than a bucket that refuses for ever or divides by nothing.
    ///
    /// The remembered takes are left alone. Narrowing therefore bites at once - a window
    /// already holding more takes than the new count refuses until enough of them age out,
    /// which is the conservative reading of "you are over the limit" - and widening frees room
    /// immediately without handing back what has already been spent.
    pub fn relimit(&mut self, limit: RateLimit) {
        self.limit = RateLimit {
            count: limit.count.max(1),
            per_secs: limit.per_secs.max(1),
        };
    }

    /// The remembered takes still inside the window at `now`, oldest first. A suffix of
    /// `taken`, because the deque is in `Instant` order.
    fn live(&self, now: Instant) -> impl Iterator<Item = &Instant> {
        let per = self.per();
        self.taken
            .iter()
            .filter(move |t| now.duration_since(**t) < per)
    }

    fn per(&self) -> Duration {
        Duration::from_secs(u64::from(self.limit.per_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Instant + Duration` panics on overflow, and `retry_after` is a number off the network.
    /// Both callers clamp it now, so this is the floor under them rather than the fix.
    #[test]
    fn a_hold_longer_than_time_itself_does_not_panic() {
        let now = Instant::now();
        let mut b = Bucket::new(HOST_START);
        b.drain(now, Duration::MAX);
        assert_eq!(
            b.peek(now),
            Some(crate::limits::MAX_RETRY_AFTER),
            "the longest hold this client will sit out"
        );
    }

    /// The seam a fixed window left open, and the whole reason this is a sliding one.
    ///
    /// One take opens the window, two more fill it at the far end of it, and a fixed window
    /// then rolls at `t0 + 1s` and hands out its whole budget again - five takes inside the
    /// second that runs from `t0 + 990ms`, against a tier of three.
    #[test]
    fn a_tier_is_never_exceeded_across_a_window_seam() {
        let t0 = Instant::now();
        let mut b = Bucket::new(RateLimit {
            count: 3,
            per_secs: 1,
        });
        assert_eq!(b.take(t0), None, "opens the window");
        assert_eq!(b.take(t0 + Duration::from_millis(990)), None);
        assert_eq!(b.take(t0 + Duration::from_millis(990)), None);
        // The first take is exactly a window old, so its slot - and only its slot - is back.
        assert_eq!(b.take(t0 + Duration::from_millis(1000)), None);
        // A fixed window would have rolled here and allowed two more.
        assert_eq!(
            b.take(t0 + Duration::from_millis(1001)),
            Some(Duration::from_millis(989)),
            "five takes inside the second that began at 990ms"
        );
        // And exactly one window after the second take, the next slot comes back.
        assert_eq!(b.take(t0 + Duration::from_millis(1990)), None);
    }

    /// Narrowing bites on the takes already remembered; widening frees room at once.
    #[test]
    fn a_relimit_applies_to_the_window_already_open() {
        let t0 = Instant::now();
        let mut b = Bucket::new(RateLimit {
            count: 10,
            per_secs: 1,
        });
        for _ in 0..4 {
            assert_eq!(b.take(t0), None);
        }
        b.relimit(RateLimit {
            count: 2,
            per_secs: 1,
        });
        assert_eq!(
            b.take(t0 + Duration::from_millis(1)),
            Some(Duration::from_millis(999)),
            "four takes stand against a budget of two"
        );
        b.relimit(RateLimit {
            count: 6,
            per_secs: 1,
        });
        assert_eq!(b.take(t0 + Duration::from_millis(1)), None);
    }

    /// A zero off the network is a budget of one, never a division by nothing.
    #[test]
    fn a_nonsense_tier_is_clamped_rather_than_obeyed() {
        let t0 = Instant::now();
        let mut b = Bucket::new(RateLimit {
            count: 0,
            per_secs: 0,
        });
        assert_eq!(
            b.limit(),
            RateLimit {
                count: 1,
                per_secs: 1
            }
        );
        assert_eq!(b.take(t0), None);
        assert_eq!(b.take(t0), Some(Duration::from_secs(1)));
    }
}
