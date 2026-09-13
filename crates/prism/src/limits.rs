//! What a Prism Central says about its own rate limits, read off every response.
//!
//! The headers below are declared in **none** of the ninety-three files under `specs/`. They
//! were found on the wire against a pc.7.6 Prism Central, on an ordinary 200:
//!
//! ```text
//! x-api-ratelimit-limit: 3        x-api-ratelimit-refresh-period-seconds: 1
//! x-api-ratelimit-remaining: 2
//! x-ratelimit-limit: 80           x-ratelimit-remaining: 79     x-ratelimit-reset: 0
//! ```
//!
//! Undeclared means advisory: a Prism Central that sends none of them must leave the client's
//! conservative assumption exactly as it was, and a value that will not parse must be worth no
//! more than an absent one. Nothing here can widen a budget past [`crate::bucket::HOST_CEILING`]
//! and nothing here can panic, which is the whole of what "read a number off the network"
//! is allowed to mean.
//!
//! A value that *will* parse is a claim too, and two of them are bounded rather than believed:
//! see [`MAX_RETRY_AFTER`] and `MAX_REFRESH_PERIOD`. The bounds are deliberately different
//! numbers, because clamping a hold and clamping a window are not the same kind of safety.
//!
//! The two pairs are different claims and are kept apart. The `x-api-ratelimit-*` triple states
//! a **tier**: a count and the period it refreshes over, which is exactly a [`RateLimit`]. The
//! `x-ratelimit-*` triple states a **budget**: how much of something is left, over a window
//! whose length the headers never give. So the first drives the bucket's tier and the second
//! only ever brakes - there is no honest way to turn "79 of 80 left" into a rate.

use std::time::Duration;

use nutsh_catalog::RateLimit;
use reqwest::header::HeaderMap;

use crate::bucket::HOST_CEILING;

/// Below this share of the longer budget the client goes to a crawl. A tenth: far enough from
/// the wall to leave room for the requests already in flight, close enough that ordinary use
/// never reaches it.
const RESERVE: u64 = 10;

/// The longest this client waits because a far end asked it to: a `Retry-After` on a 429, an
/// `x-ratelimit-reset` on an ordinary answer, or the refresh period [`Advertised::hold`] falls
/// back to when neither named an interval. One number, so there cannot be two answers to "how
/// long is too long".
///
/// Clamping a **hold** is safe in the only direction that matters: it makes the client wait
/// *less* than it was told, and the worst that costs is a 429 that it then honours. Believing
/// one is not: `x-ratelimit-reset: 9223372036854775807` is an epoch stamp where a delta was
/// meant, and `now + 292 billion years` panics in [`crate::bucket::Bucket::drain`] - on the
/// first negotiation probe, before a frame is drawn. A plausible-looking epoch value is worse
/// still, because it is a silent fifty-six-year hold rather than a crash.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// The longest refresh period this client will adopt from `x-api-ratelimit-refresh-period-\
/// seconds`.
///
/// The bound is an hour where [`MAX_RETRY_AFTER`] is a minute, and the difference is not
/// arbitrary. A period is the window a tier is spent over, so clamping it **down** shortens the
/// window and makes this client faster than the server said it may be - the one direction the
/// pacing must never be wrong in, against a Prism Central that answers an overrun with 429s and
/// a lockout policy. So it is set generously enough that no real per-hour tier can reach it,
/// and exists only to bound the absurd. It replaces an `unwrap_or(u32::MAX)` that turned any
/// period past 2^32 into a 136-year window `acquire` would have slept.
const MAX_REFRESH_PERIOD: u32 = 3600;

/// The rate limits one response advertised. Every field is `None` when the header was absent,
/// empty, or not a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Advertised {
    /// `x-api-ratelimit-limit` over `x-api-ratelimit-refresh-period-seconds`: a tier, stated.
    pub tier: Option<RateLimit>,
    /// `x-ratelimit-limit` and `x-ratelimit-remaining`: a budget over an unstated window.
    pub budget: Option<Budget>,
}

/// The longer budget: how much of it there is and how much is left. The window it spans is not
/// in the headers, so nothing here is converted into a rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub limit: u64,
    pub remaining: u64,
    /// `x-ratelimit-reset` when it was positive. A Prism Central sends `0` on a budget that is barely
    /// touched, which says nothing, so zero is `None` rather than "wait no time at all".
    pub reset: Option<Duration>,
}

impl Advertised {
    /// Read the headers of one response.
    pub fn read(headers: &HeaderMap) -> Advertised {
        let tier = match (
            number(headers, "x-api-ratelimit-limit"),
            number(headers, "x-api-ratelimit-refresh-period-seconds"),
        ) {
            // A count with no period is a tier over an unstated window, which is the one thing
            // this pair cannot be read as. It is dropped rather than assumed to be per second.
            (Some(count), Some(per_secs)) => Some(RateLimit {
                count: u32::try_from(count).unwrap_or(u32::MAX),
                // Clamped before the conversion, so nothing past `u32` has to be given a
                // meaning: see [`MAX_REFRESH_PERIOD`] for why this bound, and why down.
                per_secs: u32::try_from(per_secs.min(u64::from(MAX_REFRESH_PERIOD)))
                    .unwrap_or(MAX_REFRESH_PERIOD),
            }),
            _ => None,
        };
        let budget = match (
            number(headers, "x-ratelimit-limit"),
            number(headers, "x-ratelimit-remaining"),
        ) {
            (Some(limit), Some(remaining)) => Some(Budget {
                limit,
                remaining,
                reset: number(headers, "x-ratelimit-reset")
                    .filter(|s| *s > 0)
                    .map(|s| Duration::from_secs(s).min(MAX_RETRY_AFTER)),
            }),
            _ => None,
        };
        Advertised { tier, budget }
    }

    /// The tier to run at now: what the far end stated, or `current` when it stated nothing,
    /// capped at [`HOST_CEILING`] and braked to a crawl while the longer budget is nearly out.
    ///
    /// `current` rather than a constant, so a Prism Central whose later answers stop carrying
    /// the headers keeps the tier its earlier ones taught rather than springing back.
    pub fn ceiling(&self, current: RateLimit) -> RateLimit {
        let stated = self.tier.unwrap_or(current);
        let tier = if HOST_CEILING.tighter_than(stated) {
            HOST_CEILING
        } else {
            stated
        };
        if self.nearly_spent() {
            // One request per refresh period: the slowest this can go while still going, so a
            // view that is merely expensive degrades into a slow one rather than a stopped one.
            return RateLimit {
                count: 1,
                per_secs: tier.per_secs.max(1),
            };
        }
        tier
    }

    /// How long to hold every request, because the longer budget is spent and the next request
    /// would be the one the server has to answer with a 429.
    ///
    /// `reset` when the far end named it. Without it, one refresh period: that is the only
    /// interval these headers ever state, and holding for it re-asks the question a second
    /// later instead of inventing a window length out of nothing.
    ///
    /// Both arms are bounded by [`MAX_RETRY_AFTER`], which is what makes it one number rather
    /// than two. A period is allowed to be an hour - [`MAX_REFRESH_PERIOD`] says why, and it is
    /// the right bound for pacing - but an hour is not a hold, it is the client stopped: one
    /// `200` would freeze every request in the process, silently, and `ceiling` would keep the
    /// tier it came with for the rest of the session. Bounded, this re-asks once a minute into
    /// a budget that is still spent, and the 429 that answers is honoured with the same number.
    pub fn hold(&self, current: RateLimit) -> Option<Duration> {
        let budget = self.budget?;
        if budget.remaining > 0 {
            return None;
        }
        Some(
            budget
                .reset
                .unwrap_or_else(|| Duration::from_secs(u64::from(current.per_secs.max(1))))
                .min(MAX_RETRY_AFTER),
        )
    }

    /// Whether the longer budget is down to its last tenth. A budget of nought is not "nearly
    /// spent" but wholly spent, and [`Advertised::hold`] has that case.
    fn nearly_spent(&self) -> bool {
        let Some(b) = self.budget else {
            return false;
        };
        b.limit > 0 && b.remaining > 0 && b.remaining <= (b.limit / RESERVE).max(1)
    }
}

/// One header as a non-negative integer, or `None` for absent, empty, non-ASCII, negative,
/// fractional or oversized. A number from the network is a claim, not a fact.
fn number(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    /// The exact headers A recorded Prism Central sent, read back.
    #[test]
    fn the_labs_own_headers_read_as_three_a_second() {
        let a = Advertised::read(&headers(&[
            ("x-api-ratelimit-limit", "3"),
            ("x-api-ratelimit-refresh-period-seconds", "1"),
            ("x-api-ratelimit-remaining", "2"),
            ("x-ratelimit-limit", "80"),
            ("x-ratelimit-remaining", "79"),
            ("x-ratelimit-reset", "0"),
        ]));
        assert_eq!(
            a.tier,
            Some(RateLimit {
                count: 3,
                per_secs: 1
            })
        );
        assert_eq!(
            a.budget,
            Some(Budget {
                limit: 80,
                remaining: 79,
                // `0` says nothing, so it is not read as "wait for no time".
                reset: None,
            })
        );
        assert_eq!(
            a.ceiling(HOST_CEILING),
            RateLimit {
                count: 3,
                per_secs: 1
            }
        );
        assert_eq!(a.hold(HOST_CEILING), None);
    }

    #[test]
    fn an_answer_that_carries_nothing_changes_nothing() {
        let a = Advertised::read(&headers(&[]));
        assert_eq!(a, Advertised::default());
        assert_eq!(
            a.ceiling(crate::bucket::HOST_START),
            crate::bucket::HOST_START
        );
        assert_eq!(a.hold(crate::bucket::HOST_START), None);
    }

    /// Every way a header can lie. None of them may widen the budget, and none may panic.
    #[test]
    fn a_value_that_will_not_parse_is_worth_no_more_than_an_absent_one() {
        for value in [
            "",
            " ",
            "many",
            "-1",
            "3.5",
            "0x3",
            "99999999999999999999999",
        ] {
            let a = Advertised::read(&headers(&[
                ("x-api-ratelimit-limit", value),
                ("x-api-ratelimit-refresh-period-seconds", "1"),
            ]));
            assert_eq!(a.tier, None, "{value:?}");
            assert_eq!(
                a.ceiling(crate::bucket::HOST_START),
                crate::bucket::HOST_START,
                "{value:?}"
            );
        }
    }

    /// A count with no period could be per second, per minute or per day. It is dropped.
    #[test]
    fn a_count_without_its_period_is_not_a_tier() {
        let a = Advertised::read(&headers(&[("x-api-ratelimit-limit", "3")]));
        assert_eq!(a.tier, None);
    }

    #[test]
    fn no_advertisement_can_widen_the_budget_past_the_cap() {
        let a = Advertised::read(&headers(&[
            ("x-api-ratelimit-limit", "100000"),
            ("x-api-ratelimit-refresh-period-seconds", "1"),
        ]));
        assert_eq!(a.ceiling(crate::bucket::HOST_START), HOST_CEILING);
    }

    /// The last tenth of the longer budget is a crawl, and nought of it is a hold.
    #[test]
    fn the_longer_budget_brakes_and_then_holds() {
        let brake = |remaining: u64| {
            Advertised::read(&headers(&[
                ("x-api-ratelimit-limit", "30"),
                ("x-api-ratelimit-refresh-period-seconds", "1"),
                ("x-ratelimit-limit", "80"),
                ("x-ratelimit-remaining", &remaining.to_string()),
            ]))
        };
        assert_eq!(
            brake(79).ceiling(HOST_CEILING),
            RateLimit {
                count: 30,
                per_secs: 1
            }
        );
        assert_eq!(
            brake(8).ceiling(HOST_CEILING),
            RateLimit {
                count: 1,
                per_secs: 1
            },
            "a tenth left is a crawl"
        );
        assert_eq!(brake(8).hold(HOST_CEILING), None);
        assert_eq!(brake(0).hold(HOST_CEILING), Some(Duration::from_secs(1)));
    }

    /// A tiny budget still has a reserve: integer division would make a tenth of nine zero, and
    /// a threshold of zero is no threshold at all.
    #[test]
    fn a_small_budget_still_reserves_one() {
        let a = Advertised::read(&headers(&[
            ("x-ratelimit-limit", "4"),
            ("x-ratelimit-remaining", "1"),
        ]));
        assert!(a.nearly_spent());
    }

    #[test]
    fn a_named_reset_is_what_the_hold_waits() {
        let a = Advertised::read(&headers(&[
            ("x-ratelimit-limit", "80"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "45"),
        ]));
        assert_eq!(a.hold(HOST_CEILING), Some(Duration::from_secs(45)));
    }

    /// The two numbers a Prism Central can state that this client would otherwise believe
    /// whole, and the direction each is allowed to be wrong in.
    ///
    /// A `reset` of `i64::MAX` is what an epoch timestamp looks like where a delta was meant.
    /// Believed, it was `now + 292_000_000_000 years`, which panicked in `Bucket::drain` on the
    /// first negotiation probe - before a frame was drawn, so `--check`, `--snapshot` and the
    /// TUI all died on it. Clamping a *hold* down means waiting less than asked, which costs at
    /// worst a 429 the client then honours.
    ///
    /// A period is the other direction. Clamping it down shortens the window and makes this
    /// client **faster than the server allows**, which is the one thing the pacing may never
    /// be, so the bound is generous enough that no real per-hour tier can reach it - and the
    /// old `unwrap_or(u32::MAX)` turned any period past 2^32 into a 136-year window that
    /// `acquire` would have slept.
    /// The hold with no `reset` to read. `MAX_REFRESH_PERIOD` lets a period be an hour, which
    /// is the right bound for pacing and the wrong one for a hold: one `200` carrying a spent
    /// budget and an hour-long period would stop every request in the process, and `ceiling`
    /// would keep that tier for the rest of the session.
    #[test]
    fn a_hold_with_no_reset_to_read_is_bounded_like_every_other_hold() {
        let spent = |per_secs: &str| {
            Advertised::read(&headers(&[
                ("x-api-ratelimit-limit", "3"),
                ("x-api-ratelimit-refresh-period-seconds", per_secs),
                ("x-ratelimit-limit", "80"),
                ("x-ratelimit-remaining", "0"),
                // `0` says nothing, so there is no `reset` and the fallback is what answers.
                ("x-ratelimit-reset", "0"),
            ]))
        };
        let hour = spent("3600");
        assert_eq!(
            hour.budget.expect("a budget").reset,
            None,
            "nothing to read"
        );
        assert_eq!(
            hour.hold(RateLimit {
                count: 3,
                per_secs: MAX_REFRESH_PERIOD,
            }),
            Some(MAX_RETRY_AFTER),
            "a minute, not the hour the period would have bought"
        );
        // A period this client would really run at is under the bound and goes through whole.
        assert_eq!(
            spent("1").hold(RateLimit {
                count: 3,
                per_secs: 1,
            }),
            Some(Duration::from_secs(1)),
            "the recording's own period is still the interval it re-asks on"
        );
    }

    #[test]
    fn a_reset_and_a_period_off_the_wire_are_both_bounded() {
        let a = Advertised::read(&headers(&[
            ("x-ratelimit-limit", "80"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "9223372036854775807"),
        ]));
        assert_eq!(
            a.hold(HOST_CEILING),
            Some(MAX_RETRY_AFTER),
            "a minute, not 292 billion years"
        );

        let period = |secs: &str| {
            Advertised::read(&headers(&[
                ("x-api-ratelimit-limit", "3"),
                ("x-api-ratelimit-refresh-period-seconds", secs),
            ]))
            .tier
            .expect("a tier")
            .per_secs
        };
        assert_eq!(period("1"), 1, "the recording's own period is untouched");
        assert_eq!(period("3600"), MAX_REFRESH_PERIOD, "and an hour is allowed");
        assert_eq!(period("3601"), MAX_REFRESH_PERIOD);
        assert_eq!(
            period("4294967296"),
            MAX_REFRESH_PERIOD,
            "past what a u32 holds, which must not saturate to u32::MAX"
        );
        assert_eq!(period("18446744073709551615"), MAX_REFRESH_PERIOD);
    }
}
