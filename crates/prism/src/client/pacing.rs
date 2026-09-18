//! Pacing: the per-endpoint budgets, the host ceiling, and what a response's rate headers do to them.

use super::*;

/// The host ceiling and one bucket per operation.
#[derive(Debug)]
pub(super) struct Buckets {
    /// Every request passes here first, whatever its own tier says.
    pub(super) host: Bucket,
    /// Keyed by method and path template: the server meters per template (every VM shares the
    /// power-off budget) but declares a tier per operation, and `/vms/{extId}` reads at two a
    /// second while its PUT and DELETE run at five.
    pub(super) per_op: HashMap<(HttpMethod, &'static str), Bucket>,
}

impl Client {
    /// The budgets are shared state with no invariant worth protecting, so a poisoned lock is
    /// taken as it stands rather than panicking a whole session's requests: the worst a panic
    /// under the guard can leave behind is one bucket's counter.
    pub(super) fn buckets(&self) -> std::sync::MutexGuard<'_, Buckets> {
        self.buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait for a token on this operation's budget and on the host ceiling; never holds the lock
    /// across an await. Both are peeked before either is spent, so a caller parked on a tight
    /// tier does not burn host tokens it never uses.
    pub(super) async fn acquire(
        &self,
        method: &HttpMethod,
        template: &'static str,
        limit: RateLimit,
    ) -> bool {
        let mut slept = false;
        loop {
            let wait = {
                let mut guard = self.buckets();
                let buckets = &mut *guard;
                let now = std::time::Instant::now();
                let op = buckets
                    .per_op
                    .entry((method.clone(), template))
                    .or_insert_with(|| Bucket::new(limit));
                // The longer of the two waits, or `None` when neither refuses.
                match buckets.host.peek(now).max(op.peek(now)) {
                    None => {
                        buckets.host.take(now);
                        op.take(now);
                        None
                    }
                    some => some,
                }
            };
            let Some(w) = wait else { return slept };
            slept = true;
            // The reactive half logs its 429 waits; a caller frozen by the preventive half must
            // not be the one silent stall in a trace - nor the one invisible one on screen.
            self.metrics.record_throttled(std::time::Instant::now());
            if w >= Duration::from_secs(1) {
                tracing::warn!(%method, template, ?w, "paced by the local rate budget");
            } else {
                tracing::debug!(%method, template, ?w, "paced by the local rate budget");
            }
            tokio::time::sleep(w).await;
        }
    }

    /// A 429 arrived: hold this operation's budget and the host ceiling empty for what the server
    /// asked, so concurrent callers do not pile into the same wall. The retry `send` makes skips
    /// `acquire` on purpose: the wait was already slept.
    pub(super) fn drain_bucket(
        &self,
        method: &HttpMethod,
        template: &'static str,
        retry_after: Duration,
    ) {
        let now = std::time::Instant::now();
        self.metrics.record_rate_limited(now);
        let mut guard = self.buckets();
        let buckets = &mut *guard;
        buckets.host.drain(now, retry_after);
        if let Some(b) = buckets.per_op.get_mut(&(method.clone(), template)) {
            b.drain(now, retry_after);
        }
    }

    /// The debt on one operation's budget in the current window, and the only way a test can
    /// see the pacing without waiting for it. `method` is an HTTP method name, `template` a
    /// catalog path template.
    ///
    /// Public only because `tests/actions.rs` is a separate crate; not a supported accessor.
    #[doc(hidden)]
    pub fn bucket_debt(&self, method: &str, template: &str) -> u32 {
        // A find, not a lookup: the map is keyed by `&'static str` and a test's template is not.
        self.buckets()
            .per_op
            .iter()
            .find(|((m, t), _)| m.as_str() == method && *t == template)
            .map_or(0, |(_, b)| b.debt())
    }

    /// The tier in force for one operation: the catalog's, or the tighter one that operation's
    /// own answers advertised. See `bucket_debt` for why this is a find rather than a lookup.
    #[doc(hidden)]
    pub fn bucket_limit(&self, method: &str, template: &str) -> Option<RateLimit> {
        self.buckets()
            .per_op
            .iter()
            .find(|((m, t), _)| m.as_str() == method && *t == template)
            .map(|(_, b)| b.limit())
    }

    /// The debt on the host ceiling in the current window; see `bucket_debt`.
    #[doc(hidden)]
    pub fn host_debt(&self) -> u32 {
        self.buckets().host.debt()
    }

    /// The host tier in force: the conservative start, or what a response advertised.
    ///
    /// Public only so a test can read the pacing without waiting for it; not a supported
    /// accessor.
    #[doc(hidden)]
    pub fn host_limit(&self) -> RateLimit {
        self.buckets().host.limit()
    }

    /// How many requests go out at once: never more than the host tier allows in one refresh
    /// period, never more than [`MAX_FAN_OUT`]. Negotiation fires a probe per namespace, and a
    /// burst of four against three a second is over the limit before an answer is back.
    #[doc(hidden)]
    pub fn fan_out(&self) -> usize {
        let per_period = usize::try_from(self.host_limit().count).unwrap_or(MAX_FAN_OUT);
        per_period.clamp(1, MAX_FAN_OUT)
    }

    /// Adopt what one response said about the rate limits. Called for every answer, a 404 and a
    /// 429 included: the headers ride on all of them. An answer that advertises nothing leaves the
    /// tier as it was ([`Advertised::ceiling`]).
    pub(super) fn observe_limits(
        &self,
        headers: &HeaderMap,
        method: &HttpMethod,
        template: &'static str,
    ) {
        let advertised = Advertised::read(headers);
        let now = std::time::Instant::now();
        let mut guard = self.buckets();
        let before = guard.host.limit();
        let after = advertised.ceiling(before);
        if after != before {
            if after.tighter_than(before) {
                tracing::info!(
                    count = after.count,
                    per_secs = after.per_secs,
                    "the Prism Central advertises a tighter rate limit; slowing to it"
                );
            } else {
                tracing::debug!(
                    count = after.count,
                    per_secs = after.per_secs,
                    "adopting the advertised rate limit"
                );
            }
            guard.host.relimit(after);
        }
        // Per operation as well: a Prism Central states a tier per endpoint (pc.7.6 states ten,
        // from two a second to thirty) and the host bucket holds whichever answer spoke last, so a
        // `30/s` endpoint would widen the bucket a `3/s` endpoint's requests then pass through.
        // Tightening only: the catalog's rate is the ceiling and a header may lower it, never
        // raise it. Widening from the wire is how a client runs over a gateway's limit for a
        // whole session.
        if let Some(stated) = advertised.tier
            && let Some(op) = guard.per_op.get_mut(&(method.clone(), template))
            && stated.tighter_than(op.limit())
        {
            tracing::info!(
                %method,
                template,
                count = stated.count,
                per_secs = stated.per_secs,
                "this endpoint advertises a tighter rate limit than the catalog; slowing to it"
            );
            op.relimit(stated);
        }
        // The longer budget is spent: hold before the request that would have earned the 429,
        // rather than after it. `drain` is the same hold a served 429 sets, so the two cannot
        // disagree about what "wait" means.
        if let Some(hold) = advertised.hold(after) {
            tracing::warn!(?hold, "the request budget is spent; holding");
            guard.host.drain(now, hold);
        }
    }
}
