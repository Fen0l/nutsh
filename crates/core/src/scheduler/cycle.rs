//! One subscription's task: the cycle, the walk over pages, the backoff, and what a failure means.

use super::*;

pub(super) const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Pages one cycle walks at most. An interactive table never needs more than 20 000 rows; the
/// walk returns `Ok(())` at the cap, so the partial list still completes and the header's
/// `rows`/`total` shows the truncation instead of the screen hanging on a paging loop.
pub(super) const MAX_PAGES: u32 = 200;

pub(super) async fn run(
    client: Arc<dyn Source>,
    tx: mpsc::Sender<Msg>,
    epoch: u64,
    id: SubId,
    sub: Subscription,
    controls: Arc<Controls>,
    generations: Arc<AtomicU64>,
) {
    let mut backoff = poll_interval(sub.interval);
    loop {
        let dirty = controls.probe.take_dirty();
        let Some((mut generation, mut error)) = attempt(
            client.as_ref(),
            &tx,
            id,
            &sub,
            &controls,
            &generations,
            dirty,
        )
        .await
        else {
            return;
        };
        // A list 404 is not the endpoint's absence yet: a restored pin may route at a version this
        // PC does not serve, and `Client::repair_after` re-negotiates inside the failed call, so
        // the request behind a 404 routes to the version that answers. One immediate second
        // attempt, inside the same cycle; only a second 404 is the endpoint's own.
        if is_missing_list(&sub, error.as_ref()) {
            let Some(second) = attempt(
                client.as_ref(),
                &tx,
                id,
                &sub,
                &controls,
                &generations,
                true,
            )
            .await
            else {
                return;
            };
            (generation, error) = second;
        }
        let not_served = is_missing_list(&sub, error.as_ref());
        // What the server answered about the endpoint, kept where everything that would
        // *volunteer* the same request can see it - a page's panes, the stats counters, the
        // name warm-up, the DR sampler - rather than only on the table that paid for the 404.
        // A cycle that completes takes it back, which is what makes `^r` a way out and not a
        // formality.
        if not_served {
            client.mark_missing(sub.key.kind);
        } else if error.is_none() && is_endpoint_list(&sub) {
            client.forget_missing(sub.key.kind);
        }
        let failure = error
            .as_ref()
            .map(|e| Failure::of(sub.key.kind, e, not_served));
        let msg = match &failure {
            None => Msg::Complete {
                sub: id,
                key: sub.key.clone(),
                generation,
            },
            Some(f) => Msg::Error {
                sub: id,
                key: sub.key.clone(),
                generation,
                error: f.clone(),
            },
        };
        if tx.send(msg).await.is_err() {
            return;
        }
        if sub.once {
            // Last, and only now: the receiver reaps the entry when it drains this, by which
            // time every message of the cycle is already in front of it in the channel.
            let _ = tx.send(Msg::Done { epoch, sub: id }).await;
            return;
        }
        // Twice over, the same answer, and it would be the same answer every cycle after
        // that: a retry loop would run for ever behind a screen nobody is watching. The table
        // keeps the flag and draws the reason, and `Scheduler::refresh` is what starts this
        // task again for a person who asks.
        if not_served {
            return;
        }
        // And for a credential this Prism Central refused: `Client::send` already puts nothing on
        // the wire, but a task with nothing left to do stops. The table keeps its rows either way.
        if failure.as_ref().is_some_and(Failure::is_terminal) {
            return;
        }
        // Per round rather than once before the loop, so a back-off is clamped against the
        // interval that is current - which is what a person who has just slowed a failing view
        // expects. `max(1)` because 0 means `off`, and `wait_for_cycle` is what reads that.
        let interval = poll_interval(Duration::from_secs(u64::from(
            controls.interval.load(Ordering::Relaxed).max(1),
        )));
        let (wait, next) = next_backoff(interval, backoff, error.as_ref());
        backoff = next;
        wait_for_cycle(sub.pausable, &controls, wait).await;
    }
}

/// Waits until this subscription's next cycle: `wait`, or while idle and pausable, until
/// something wakes it - on the `Notify` it already holds, not a sleep. The flag is read at
/// both ends of the sleep: the pause is set one second at a time, and a task committed to its
/// interval must not buy one more cycle with it.
pub(super) async fn wait_for_cycle(pausable: bool, controls: &Controls, first: Duration) {
    let mut wait = first;
    loop {
        // Three ways to be waiting for a wake rather than for a clock, and they are one
        // condition: a kind with no schedule at all (`ctrl-t`'s `off`), a walk the user stopped
        // by hand (`ctrl-x`), and a session the idle pause has stopped. The first two are the
        // view's own silence and outrank the third, which is why neither consults `pausable`.
        let manual = controls.interval.load(Ordering::Relaxed) == 0;
        if manual
            || controls.stop.load(Ordering::Relaxed)
            || (pausable && controls.idle.load(Ordering::Relaxed))
        {
            controls.wake.notified().await;
        } else {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {
                    // The idle flag is read at both ends of the sleep: the pause is set one
                    // second at a time, so on any longer interval a task is almost always
                    // already asleep when it fires, and one committed to its interval must not
                    // buy one more cycle with it.
                    if !(pausable && controls.idle.load(Ordering::Relaxed)) {
                        return;
                    }
                    continue;
                }
                _ = controls.wake.notified() => {}
            }
        }
        // Only an **armed** wake ends the wait. An unarmed one is a schedule that changed
        // underneath us, so the interval is re-read and the wait starts again on the new one:
        // that is what stops an `off` issued while this task sleeps from costing one last
        // cycle, and it is why `ctrl-r` on a manual subscription buys exactly one cycle - the
        // loop comes straight back here.
        if controls.armed.swap(false, Ordering::Relaxed) {
            return;
        }
        wait = poll_interval(Duration::from_secs(u64::from(
            controls.interval.load(Ordering::Relaxed),
        )));
    }
}

/// A 404 on a top-level list path: the answer is about this Prism Central, not a row. A
/// child list's 404 carries its parents' ids (`TableKey::under`) and is a gone parent;
/// a single entity's is a gone row. Both are ordinary errors, retried like any other.
pub(super) fn is_missing_list(sub: &Subscription, error: Option<&PrismError>) -> bool {
    is_endpoint_list(sub) && matches!(error, Some(PrismError::NotFound(_)))
}

/// A subscription over the kind's **own** list path, with no parent ids in it and no single
/// entity: the one whose answer is about the endpoint rather than about a row. Both the 404
/// that is remembered and the completion that forgets it are judged on it, so they cannot
/// come to disagree about which subscription speaks for the endpoint.
pub(super) fn is_endpoint_list(sub: &Subscription) -> bool {
    sub.single.is_none() && sub.key.parents.is_empty()
}

/// One cycle's requests and the answer they settled on: the generation it ran under, and the
/// error if it failed. `None` is the receiver having gone away, which is the only reason a task
/// stops in the middle of a cycle rather than at the end of one.
pub(super) async fn attempt(
    client: &dyn Source,
    tx: &mpsc::Sender<Msg>,
    id: SubId,
    sub: &Subscription,
    controls: &Controls,
    generations: &AtomicU64,
    dirty: bool,
) -> Option<(u64, Option<PrismError>)> {
    let generation = generations.fetch_add(1, Ordering::Relaxed) + 1;
    let result = match &sub.single {
        Some(ext_id) => fetch_one(client, tx, id, sub, generation, ext_id).await,
        None => {
            // Before the first request, so the frame can say `listing…` and anchor the elapsed
            // time while the first page is still on the wire. Only a listing cycle: a `get_in`
            // has one request and nothing to walk.
            tx.send(Msg::Started {
                sub: id,
                key: sub.key.clone(),
                generation,
            })
            .await
            .ok()?;
            cycle(client, tx, id, sub, controls, generation, dirty).await
        }
    };
    Some((generation, result.err()))
}

/// A subscription's interval as the whole seconds `Controls` stores, floored: a caller that
/// passes zero must not turn its own subscription off by accident. 0 in `Controls` means `off`
/// and is only ever written by [`Scheduler::set_interval`].
pub(super) fn secs_of(interval: Duration) -> u32 {
    u32::try_from(poll_interval(interval).as_secs()).unwrap_or(u32::MAX)
}

/// The interval a cycle actually waits.
pub(super) fn poll_interval(interval: Duration) -> Duration {
    interval.max(MIN_INTERVAL)
}

/// How long to wait before the next cycle, and the back-off to carry into it. A success
/// resets both to the interval. A failure waits the back-off it arrived with - so the first
/// failure waits exactly the interval - and doubles it for the next one, never below the
/// interval and never above a minute. `RateLimited` waits what the server asked when that is
/// longer than the back-off.
pub(super) fn next_backoff(
    interval: Duration,
    current: Duration,
    error: Option<&PrismError>,
) -> (Duration, Duration) {
    let Some(e) = error else {
        return (interval, interval);
    };
    // Whatever the far end asked to be left for - a 429's `Retry-After`, and a 503's, which
    // a Prism Central under load does send - but never less than the back-off already earned.
    let wait = match e.retry_after() {
        Some(asked) => asked.max(current),
        None => current,
    };
    (
        wait,
        (current * 2).clamp(interval, MAX_BACKOFF.max(interval)),
    )
}

/// Whether the walk fetches another page. A kind that does not page, an empty page, the row
/// budget and the page cap all end it; then a known total decides, and without one only a
/// full page suggests more. `budget` is `max_rows`: a collection the PC never trims costs a
/// fixed handful of pages a cycle.
pub(super) fn more_pages(
    pages: bool,
    count: u64,
    limit: u64,
    fetched: u64,
    total: Option<u64>,
    page: u32,
    budget: Option<u64>,
) -> bool {
    pages
        && count > 0
        && page + 1 < MAX_PAGES
        && budget.is_none_or(|b| fetched < b)
        && match total {
            Some(t) => fetched < t,
            None => count == limit,
        }
}

/// What a walk cost and what it saw, for the probe.
pub(crate) struct Walk {
    pub(super) pages: u32,
    /// The largest `probe_by` value across every row of the walk, for calibration. `None` when
    /// the walk was not scanned for one, when it returned no row, and when no row it returned
    /// carried a value [`instant_of`] could read - which is why `saw_rows` is beside it.
    pub(super) max_sort: Option<String>,
    /// Whether the walk returned any row at all. A calibrating walk that read rows and found no
    /// readable value among them has shown this Prism Central answers something the probe can
    /// never compare; one over an empty collection has shown nothing.
    pub(super) saw_rows: bool,
}

/// One list cycle, with the probe when the kind carries a curated `probe_by`, the table is
/// not `Disarmed`, and its last walk took two or more pages - a one-page table's probe costs
/// what its walk costs. Whether this PC honours the order is [`calibrate`]'s check.
pub(super) async fn cycle(
    client: &dyn Source,
    tx: &mpsc::Sender<Msg>,
    id: SubId,
    sub: &Subscription,
    controls: &Controls,
    generation: u64,
    dirty: bool,
) -> Result<(), PrismError> {
    let probe_cell = &controls.probe;
    let Some(field) = sub.key.kind.probe_by else {
        let walk = list(client, tx, id, sub, controls, generation, None).await?;
        probe_cell.set_pages(walk.pages);
        return Ok(());
    };
    let state = probe_cell.state();
    if state == ProbeState::Disarmed || probe_cell.pages() < 2 {
        // `None`, not the field: neither branch calibrates, and scanning every row of every
        // page for a maximum nothing will read is per-row work on every cycle for ever.
        let walk = list(client, tx, id, sub, controls, generation, None).await?;
        probe_cell.set_pages(walk.pages);
        return Ok(());
    }
    if state == ProbeState::Unarmed {
        // The calibrating cycle walks regardless, and asks the probe after the last page: a probe
        // issued first is older than the walk, so an entity updated during the walk would look
        // newer than the probe's row and disarm a PC that sorts. Asked last, the answer is at
        // least as recent as anything the walk saw.
        let walk = list(client, tx, id, sub, controls, generation, Some(field)).await?;
        let answer = probe(client, sub, field).await?;
        let now = std::time::Instant::now();
        probe_cell.set_pages(walk.pages);
        probe_cell.set(calibrate(&state, &answer, Some(&walk)));
        client
            .metrics()
            .record_avoidable(now, u64::from(walk.pages) + 1);
        return Ok(());
    }
    // Armed, so the probe goes first: deciding whether the walk happens at all is the whole
    // point of it. A probe is a real request - it spends a token on the kind's tier and it
    // counts in `made`.
    let answer = probe(client, sub, field).await?;
    let now = std::time::Instant::now();
    if probe_decision(&state, &answer, dirty) == ProbeDecision::Skip {
        probe_cell.skipped();
        // The pages the walk did not take, beside the one request the probe did make: the same
        // `pages + 1` universe the walking branch below records, so a skip and the walk it
        // replaced are scored against one denominator.
        client
            .metrics()
            .record_avoided(now, u64::from(probe_cell.pages()));
        client.metrics().record_avoidable(now, 1);
        // Nothing staged: `run` sends `Complete`, and `Store::apply` already handles a
        // `Complete` for a generation whose pages are not the ones staged - rows untouched,
        // `last_poll` refreshed, `error` cleared, `generation` where it was, so the next real
        // walk still lands and `● live` stays green because a cycle did complete.
        return Ok(());
    }
    // The sort check is asked once, above. Here the probe goes first - deciding whether the walk
    // happens is the point - so it is older than the walk, and re-checking would disarm the
    // busiest tables on a benign race. `calibrate` is handed no walk once armed.
    let walk = list(client, tx, id, sub, controls, generation, None).await?;
    probe_cell.set_pages(walk.pages);
    probe_cell.set(calibrate(&state, &answer, None));
    client
        .metrics()
        .record_avoidable(now, u64::from(walk.pages) + 1);
    Ok(())
}

/// One `$limit=1` list ordered by `field` descending: the probe's own question, asked with the
/// table's filter and parents so it sees the same collection the walk would.
pub(super) async fn probe(
    client: &dyn Source,
    sub: &Subscription,
    field: &str,
) -> Result<ProbeAnswer, PrismError> {
    let opts = ListOptions {
        limit: 1,
        parents: sub.key.parents.clone(),
        filter: sub.filter.clone(),
        orderby: Some(format!("{field} desc")),
        ..Default::default()
    };
    let page = client.list_page(sub.key.kind, 0, &opts).await?;
    let first = page.entities.first();
    Ok(ProbeAnswer {
        sort_value: first
            .and_then(|e| e.raw.get(field))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        ext_id: first.map(|e| e.ext_id.clone()),
        total: page.total,
    })
}

/// A cycle's pages, in order, each pushed as it arrives. `probe_by` is the field whose newest
/// value across the walk calibrates the probe, and it is `Some` only on the cycle that
/// calibrates: a kind with no `probe_by`, a disarmed table and every cycle past the first armed
/// one pass `None`, and pay for no per-row scan.
pub(super) async fn list(
    client: &dyn Source,
    tx: &mpsc::Sender<Msg>,
    id: SubId,
    sub: &Subscription,
    controls: &Controls,
    generation: u64,
    probe_by: Option<&str>,
) -> Result<Walk, PrismError> {
    let opts = ListOptions {
        limit: sub.page_size.clamp(1, PAGE_LIMIT),
        parents: sub.key.parents.clone(),
        filter: sub.filter.clone(),
        // The kind's curated order unless this subscription named one: a page pane keeps its
        // own, and every other list of a kind with a budget gets the order that makes the
        // rows the budget keeps the ones worth keeping.
        orderby: sub
            .orderby
            .clone()
            .or_else(|| sub.key.kind.orderby.map(str::to_string)),
        // The one place a `$select` is ever set. `can_i` and the version probes reach
        // `Client::list_page_at` with default options, and a narrowing applied down there would
        // drop `identities[].identityFilter` from the authorisation lists and grey every action
        // on every kind as not permitted, without a word. So the scheduler says it, on the
        // cycle that fills a table, and nothing else does.
        select: sub.key.kind.select.map(str::to_string),
    };
    let limit = u64::from(opts.limit);
    let budget = sub.max_rows.map(u64::from);
    let mut page = 0u32;
    let mut fetched = 0u64;
    let mut walk = Walk {
        pages: 0,
        max_sort: None,
        saw_rows: false,
    };
    // The parsed twin of `walk.max_sort`: the comparison is between instants, never between
    // bytes, for the reason [`calibrate`] gives.
    let mut newest: Option<time::OffsetDateTime> = None;
    loop {
        let result = client.list_page(sub.key.kind, page, &opts).await?;
        let count = result.entities.len() as u64;
        let total = result.total;
        fetched += count;
        walk.pages += 1;
        walk.saw_rows |= count > 0;
        if let Some(field) = probe_by {
            for e in &result.entities {
                // A row whose value will not parse is a row this comparison cannot use; the
                // generator has already refused a `probe_by` the schema does not declare as a
                // timestamp, so an unparseable value is a Prism Central answering something
                // else, and calibration below sees an unparseable probe answer for itself.
                let Some(at) = e
                    .raw
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|seen| instant_of(seen).map(|at| (seen, at)))
                else {
                    continue;
                };
                if newest.is_none_or(|max| max < at.1) {
                    newest = Some(at.1);
                    walk.max_sort = Some(at.0.to_string());
                }
            }
        }
        // Nobody is draining any more: stop the walk instead of fetching the rest for a
        // receiver that is gone. `run` returns when its own send fails.
        if tx
            .send(Msg::Page {
                sub: id,
                key: sub.key.clone(),
                generation,
                entities: result.entities,
                total,
            })
            .await
            .is_err()
        {
            return Ok(walk);
        }
        // Stopped by hand, after the page rather than before the request that answered it: the
        // pages that arrived are staged, and `run` sends the `Complete` that lands them.
        if controls.stop.load(Ordering::Relaxed) {
            return Ok(walk);
        }
        if !more_pages(
            sub.key.kind.list_params.page,
            count,
            limit,
            fetched,
            total,
            page,
            budget,
        ) {
            return Ok(walk);
        }
        page += 1;
    }
}

/// One `get_in` for a `single` subscription: the parents fill the path, the ext id the last
/// placeholder. The `Entity` it pushes updates one row without disturbing the list's cycle.
pub(super) async fn fetch_one(
    client: &dyn Source,
    tx: &mpsc::Sender<Msg>,
    id: SubId,
    sub: &Subscription,
    generation: u64,
    ext_id: &str,
) -> Result<(), PrismError> {
    let entity = client
        .get_in(sub.key.kind, &sub.key.parents, ext_id)
        .await?;
    if tx
        .send(Msg::Entity {
            sub: id,
            key: sub.key.clone(),
            generation,
            entity,
        })
        .await
        .is_err()
    {
        return Ok(());
    }
    Ok(())
}
