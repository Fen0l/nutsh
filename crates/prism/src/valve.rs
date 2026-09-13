//! The last resort: a hard cap on how often this client may have a credential refused.
//!
//! Everything else in this crate is a rule about what the client *concludes* - that a 401 on a
//! password-bearing request means the password is wrong, that a 401 on a cookie-only one means
//! the session ended, that a refused credential is never presented again. Those rules are the
//! fix. This is what holds when one of them is wrong.
//!
//! It exists because of what a lockout actually is. An account is locked by **failed**
//! authentications: a Prism Central, or the directory behind it, counts consecutive failures
//! and locks at a threshold, and a successful authentication resets that counter. So the thing
//! that has to be structurally impossible is not authenticating often - it is authenticating
//! *wrongly* often. [`Valve`] therefore counts refusals, not presentations, and latches on
//! [`PRESENTATIONS`] of them inside [`WINDOW`]. Once latched, nothing goes out at all: not the
//! credential, not a request that would have ridden a session, not a negotiation probe.
//!
//! The cap is deliberately below any plausible lockout threshold and far above anything a
//! healthy session does, which is **nought**: the state machine stops at the first refusal, so
//! this counter should never reach two. If it ever does, something upstream is broken and
//! stopping is the right answer - showing a stale table is vastly better than locking out a
//! production administrator.
//!
//! Two things it deliberately does **not** count. A transport failure, because the credential
//! never reached an authenticator that could have counted it and a network outage must not end
//! a session. And a presentation the far end *accepted*, because a successful authentication
//! cannot lock an account - a Prism Central that issues no session cookie leaves every request
//! carrying the credential, which is what this client did before session reuse existed, and
//! capping that would brick the tool against such a Prism Central to prevent a harm that does
//! not exist. What bounds that traffic is the rate ceiling in `bucket`, not this.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Refusals inside [`WINDOW`] before the valve latches.
pub const PRESENTATIONS: usize = 3;

/// The window the refusals are counted over.
pub const WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug, Default)]
pub struct Valve {
    /// A `Mutex` held for nanoseconds and never across an await, on the precedent of
    /// `Client::buckets`.
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// When each refusal still inside the window happened, oldest first.
    refusals: VecDeque<Instant>,
    latched: bool,
}

impl Valve {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned valve is taken as it stands, and the conservative reading is the one
        // already there: a panic under the guard cannot un-latch it.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether this client may make a request at all.
    pub fn open(&self) -> bool {
        !self.inner().latched
    }

    /// Shut the valve for the life of this client, whatever the counter says. What a credential
    /// the far end has refused outright does immediately.
    pub fn latch(&self) {
        self.inner().latched = true;
    }

    /// A credential this client presented was refused. Latches when that is the
    /// [`PRESENTATIONS`]th refusal inside [`WINDOW`].
    pub fn refused(&self, now: Instant) {
        let mut inner = self.inner();
        while inner
            .refusals
            .front()
            .is_some_and(|t| now.duration_since(*t) >= WINDOW)
        {
            inner.refusals.pop_front();
        }
        inner.refusals.push_back(now);
        if inner.refusals.len() >= PRESENTATIONS {
            tracing::error!(
                refusals = inner.refusals.len(),
                "the credential has been refused too often; this session will make no more \
                 requests"
            );
            inner.latched = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap, and the sliding window it is counted over.
    #[test]
    fn three_refusals_inside_the_window_shut_it() {
        let t0 = Instant::now();
        let v = Valve::default();
        assert!(v.open());
        v.refused(t0);
        v.refused(t0);
        assert!(v.open(), "two is under the cap");
        v.refused(t0);
        assert!(!v.open());
    }

    /// Refusals spread thinly enough never reach the cap: the window is what makes this a rate
    /// rather than a lifetime total, so a session left running for a week is not shut by three
    /// unrelated hiccups days apart.
    #[test]
    fn refusals_a_window_apart_never_add_up() {
        let t0 = Instant::now();
        let v = Valve::default();
        for i in 0..10 {
            v.refused(t0 + WINDOW * i);
            assert!(v.open(), "refusal {i}");
        }
    }

    /// One refused credential shuts it outright, without waiting for the counter: the counter is
    /// the backstop, and this is what the state machine does when it is working.
    #[test]
    fn a_refused_credential_shuts_it_at_once() {
        let v = Valve::default();
        v.latch();
        assert!(!v.open());
    }

    /// And nothing reopens it.
    #[test]
    fn nothing_reopens_it() {
        let t0 = Instant::now();
        let v = Valve::default();
        v.latch();
        v.refused(t0 + WINDOW * 100);
        assert!(!v.open());
    }
}
