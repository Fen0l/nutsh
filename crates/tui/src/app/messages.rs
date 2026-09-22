//! The poll channel's receiver, and what settles it.

use super::*;

impl App {
    /// A poll message from the scheduler; stale subscriptions are dropped, the rest reach the
    /// store.
    pub fn apply(&mut self, msg: Msg) {
        // Before every arm below, because three of them move `msg` and return: these are the
        // messages that replace what a cache holds - a list cycle's rows, the seven counters,
        // the Disaster Recovery tallies - and the flag is what keeps the debounce from writing
        // the same bytes again a minute later.
        if matches!(
            msg,
            Msg::Complete { .. } | Msg::Stats { .. } | Msg::Sample { .. }
        ) {
            self.cache_dirty = true;
        }
        // Before the gate: an `Acted` belongs to no subscription, so gating it on `is_live`
        // would drop every result whose view had already been popped.
        if let Msg::Acted {
            journal,
            plan,
            result,
        } = msg
        {
            // The user just changed something and is watching for it: that table walks next
            // cycle rather than answering "nothing changed" from a probe.
            // `row_kind`, not `kind`: a refresh is issued against the row's table, not against
            // the kind the request ran on, which is why `PlanRef` carries both.
            let row_kind = plan.row_kind.id;
            // An action is somebody being there, so `woke` comes first. Both, even when it was
            // `woke` that lifted the pause: its `refresh_all` only marks and rings, while
            // `refresh_kind` also respawns a task that has *returned* - the second chance a
            // list that answered 404 gets - and that is the one thing the walk it duplicates
            // does not buy. The duplicate costs a permit `Notify` keeps and one extra cycle of
            // the table the user just acted on, which is the cycle they asked for.
            self.woke();
            if let Some(live) = self.live.as_mut() {
                for s in live.schedulers_mut() {
                    s.refresh_kind(row_kind);
                }
            }
            self.acted(journal, plan, result);
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // A `once` that has run its cycle. The subscription is reaped here, by the receiver,
        // and only after every message that cycle sent has been drained: reaping it on the
        // sender side would race this drain and could gate out the refresh the `once` exists
        // to deliver.
        if let Msg::Done { epoch, sub } = msg {
            for s in live.schedulers_mut() {
                if s.epoch() == epoch {
                    s.reap(epoch, sub);
                }
            }
            self.search_answered(sub);
            return;
        }
        // Not about a table: the page's sampler, whose cycle belongs to the session that
        // started it and to no subscription at all.
        if let Msg::Sample { generation, sample } = msg {
            if generation == self.session_generation {
                live.store.set_sample(sample);
                live.refresh_summary();
                self.sample_seen = true;
                self.dirty = true;
            }
            return;
        }
        if let Msg::Stats { generation, stats } = msg {
            if generation == self.session_generation {
                live.store.set_stats(stats);
                live.refresh_summary();
                self.stats_seen = true;
                self.dirty = true;
            }
            return;
        }
        if let Msg::Names { generation, names } = msg {
            if generation == self.session_generation {
                live.store.apply_names(names);
                self.names_seen = true;
                self.dirty = true;
            }
            return;
        }
        // `Msg::Sample`, `Msg::Stats`, `Msg::Names` and `Msg::Acted` are the only arms without a
        // subscription, and with `Msg::Done` the only ones without an update; all five returned
        // above. This guard and the `into_update` one below are exhaustive by construction,
        // kept as the routing they read as.
        let Some(sub) = msg.sub() else { return };
        if !live.any_live(sub) {
            return; // a message from a view that was popped
        }
        // The detail's single subscription shares its table's key. Only its `Entity` reaches
        // the store, so the row updates in place under the pane; its `Error` belongs to the
        // pane alone, so one missing entity never marks the whole table failed; and it stages
        // no pages, so its `Complete` says nothing.
        if let Some(detail) = self.detail.as_mut()
            && detail.sub == Some(sub)
        {
            match msg {
                Msg::Entity {
                    key,
                    generation,
                    entity,
                    ..
                } => {
                    detail.error = None;
                    live.store.apply(
                        &key,
                        nutsh_core::store::Update::Entity { generation, entity },
                    );
                    // A watched pane compares this poll with the last and lights what moved.
                    if let Some(watch) = detail.watch.as_mut()
                        && let Some(row) = live.store.table(&key).row(&detail.ext_id)
                    {
                        let sections = nutsh_core::detail::compose(
                            key.kind,
                            row,
                            live.store.names(),
                            self.now,
                        );
                        watch.observe(&sections, std::time::Instant::now());
                    }
                }
                Msg::Error { error, .. } => detail.error = Some(error),
                // A single subscription never lists, so it never announces a walk either.
                Msg::Started { .. }
                | Msg::Page { .. }
                | Msg::Complete { .. }
                | Msg::Sample { .. }
                | Msg::Stats { .. }
                | Msg::Names { .. }
                | Msg::Acted { .. }
                | Msg::Done { .. } => {}
            }
            self.dirty = true;
            return;
        }
        // A watched task's entity is an ordinary Tasks row, so the watcher reads it off the
        // same channel rather than polling one of its own. Nothing else in the table's own
        // messages can match: only extIds this session asked about are watched.
        let finished = match &msg {
            Msg::Entity { entity, .. } => live.tasks.apply(entity),
            _ => None,
        };
        if let Some(done) = &finished {
            live.unsubscribe_any(done.watch.sub);
            live.journal.settle(
                done.watch.journal,
                done.outcome.clone(),
                Some(done.watch.task.clone()),
            );
        }
        if let Some(done) = finished {
            if matches!(done.watch.status, TaskStatus::Succeeded) {
                let site = self.live.as_ref().and_then(|l| {
                    l.journal
                        .context_of(done.watch.journal)
                        .filter(|c| *c != l.primary_name())
                        .map(std::sync::Arc::from)
                });
                self.refresh_row(done.watch.row_kind, &done.watch.ext_id, site);
            }
            self.status = Some(format!(
                "{} {}: {}",
                done.watch.action,
                done.watch.name,
                done.watch.status.label()
            ));
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // A pane's message belongs to the page, not to a table view: the store still holds the
        // rows, and the summary box is rebuilt from them as they land.
        let on_page = live.page().is_some_and(|page| page.pane_of(sub).is_some());
        let Some((key, update)) = msg.into_update() else {
            return;
        };
        live.store.apply(&key, update);
        live.clamp_selection();
        // A reference picker's one-shot list. It opened on whatever the store held, so the
        // rows that land are folded straight into it, under the filter already typed.
        if let Some(form) = self.form.as_mut()
            && form
                .picker
                .as_ref()
                .is_some_and(|p| key == TableKey::top(p.kind))
        {
            let table = live.store.table(&key);
            form.loading = table.loading;
            // A cycle that failed clears `loading` too, so without this the box would say
            // "no rows" for a kind it never managed to read.
            form.listing_error = table.error.clone();
            let rows = choices(table);
            if let Some(picker) = form.picker.as_mut() {
                picker.set_rows(rows);
            }
        }
        if on_page {
            // The scheduler has already told the client what the server answered, so this asks
            // the session the same question `PageView::open` asked and acts on a different
            // answer: a pane whose list has just 404'd greys and stops polling.
            let Live {
                session,
                scheduler,
                stack,
                ..
            } = live;
            if let Some(View::Page(page)) = stack.last_mut() {
                page.regrade(scheduler, |kind| pane_reason(session, kind));
            }
            live.refresh_summary();
        }
        self.dirty = true;
    }

    /// The result of a mutation: the watch is opened, the journal entry settled, and the row
    /// it changed refreshed out of band.
    pub(super) fn acted(
        &mut self,
        journal: JournalId,
        plan: PlanRef,
        result: Result<Outcome, String>,
    ) {
        self.dirty = true;
        let outcome = result.unwrap_or_else(Outcome::failed);
        // `live` is borrowed for exactly the work that needs it, and released before the
        // status line and the refresh, which both need `&mut self`.
        let mut refresh = None;
        let status = {
            let Some(live) = self.live.as_mut() else {
                return;
            };
            // The journal recorded which context the request went to; the watch and the
            // refresh go to the same one.
            let site: Option<std::sync::Arc<str>> = live
                .journal
                .context_of(journal)
                .filter(|c| *c != live.primary_name())
                .map(std::sync::Arc::from);
            match &outcome {
                Outcome::Started(task) => {
                    // A watch needs a plan back; `PlanRef` carries everything but the body and
                    // the parents, and a watch never sends anything, so the empty parents are
                    // never used. `row_kind` is not guessed from `kind`: with `action_kind` set
                    // they differ, and the watch refreshes the row's table, not the target's.
                    let full = Plan {
                        kind: plan.kind,
                        ext_id: plan.ext_id.clone(),
                        name: plan.name.clone(),
                        action: plan.action,
                        body: None,
                        parents: Vec::new(),
                        row_kind: plan.row_kind,
                    };
                    let Live {
                        tasks,
                        scheduler,
                        peers,
                        journal: ring,
                        ..
                    } = live;
                    let owner = match site.as_deref() {
                        None => Some(scheduler),
                        Some(name) => peers
                            .iter_mut()
                            .find(|p| &*p.name == name)
                            .map(|p| &mut p.scheduler),
                    };
                    if let Some(owner) = owner {
                        tasks.watch(owner, site.clone(), &full, task.clone(), journal);
                    }
                    // The id goes in now, not when the watch settles: a running task is exactly
                    // the row a reader wants to look up in `:tasks`, and until the watch
                    // finishes the terminal settle has not run.
                    ring.settle(journal, JournalOutcome::Started, Some(task.ext_id.clone()));
                    format!("{} {}: task started", plan.action.name, plan.name)
                }
                Outcome::Done => {
                    live.journal
                        .settle(journal, JournalOutcome::Succeeded, None);
                    // The row's kind, for the same reason the watch carries it: the refresh is
                    // issued against the table the row is in.
                    refresh = Some((plan.row_kind, plan.ext_id.clone(), site.clone()));
                    format!("{} {}: done", plan.action.name, plan.name)
                }
                // The document is opened over the table below, once `live` is no longer
                // borrowed: an answer nobody can read is an action that reported nothing.
                Outcome::Returned(_) => {
                    live.journal
                        .settle(journal, JournalOutcome::Succeeded, None);
                    format!("{} {}: done", plan.action.name, plan.name)
                }
                Outcome::Failed { failure, forbidden } => {
                    // The journal is exactly where the far end's own words belong: a trace a
                    // developer reads, not a line a person is asked to act on.
                    live.journal.settle(
                        journal,
                        JournalOutcome::Failed(failure.upstream.clone()),
                        None,
                    );
                    // The answer to "may this account act" was just disproved by the server;
                    // the next caller re-resolves rather than going on greying nothing.
                    if *forbidden {
                        live.can_i.invalidate();
                    }
                    format!(
                        "{} {} failed: {}",
                        plan.action.name,
                        plan.name,
                        failure.text()
                    )
                }
            }
        };
        if let Some((kind, ext_id, site)) = refresh {
            self.refresh_row(kind, &ext_id, site);
        }
        if let Outcome::Returned(value) = outcome {
            self.open_payload(format!("{} {}", plan.action.title(), plan.name), value);
        }
        self.status = Some(status);
    }

    /// Refresh the affected entity out of band, so the row changes before the next poll cycle.
    /// `kind` is the row's own kind, never the action target's: the `once` writes into the
    /// table the row is in.
    ///
    /// Skipped when that table is not what the top view is showing - the view moved on while
    /// the request was in flight - and skipped when the kind has no `get_path`, which the flat
    /// Host does not: there is no single-entity read to issue. The row settles on the next
    /// ordinary poll instead, and the status line says the same thing either way.
    pub(super) fn refresh_row(
        &mut self,
        kind: &'static Kind,
        ext_id: &str,
        site: Option<std::sync::Arc<str>>,
    ) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(view) = live.table() else { return };
        if view.key.kind.id != kind.id || kind.get_path.is_none() {
            return;
        }
        // A merged row's refresh writes into its own context's table, asked of its own
        // Prism Central.
        let key = match (&site, view.key.context.is_none()) {
            (Some(name), true) => view.key.clone().in_context(name.clone()),
            _ => view.key.clone(),
        };
        if let Some(scheduler) = live.scheduler_for(key.context.as_deref()) {
            scheduler.subscribe(Subscription::once(key, ext_id.to_string()));
        }
    }

    /// Drains poll messages until every pane of the open page has settled; what `--snapshot`
    /// and the page tests wait for. A page with nothing to poll - every pane's namespace
    /// missing - settles at once.
    ///
    /// Like [`App::settle_once`], it waits forever if a poll never arrives: every caller wraps
    /// it in a timeout.
    pub async fn settle_page_once(&mut self) {
        let Some(page) = self.page() else { return };
        let mut waiting: Vec<SubId> = page.panes.iter().filter_map(|p| p.sub).collect();
        while !waiting.is_empty() {
            let Some(msg) = self.poll_rx.recv().await else {
                return;
            };
            if matches!(&msg, Msg::Complete { .. } | Msg::Error { .. }) {
                waiting.retain(|s| Some(*s) != msg.sub());
            }
            self.apply(msg);
        }
    }

    /// Drains poll messages, applying each, until one of them satisfies `wanted`.
    ///
    /// Like [`App::settle_once`], it waits forever if that message never arrives: every caller
    /// wraps it in a timeout.
    pub(super) async fn settle_until(&mut self, wanted: fn(&Msg) -> bool) {
        while let Some(msg) = self.poll_rx.recv().await {
            let done = wanted(&msg);
            self.apply(msg);
            if done {
                return;
            }
        }
    }

    /// Drains poll messages until every peer subscription of the top table view has completed
    /// or failed once. Returns at once with no peers. Like [`App::settle_once`], it waits
    /// forever if nothing arrives: every caller wraps it in a timeout.
    pub async fn settle_peers_once(&mut self) {
        let subs: Vec<SubId> = self
            .live
            .as_ref()
            .and_then(Live::table)
            .map(|v| v.peer_subs.iter().map(|(_, s)| *s).collect())
            .unwrap_or_default();
        let mut waiting: std::collections::HashSet<SubId> = subs.into_iter().collect();
        while !waiting.is_empty() {
            let Some(msg) = self.poll_rx.recv().await else {
                return;
            };
            if let (Some(sub), true) = (
                msg.sub(),
                matches!(msg, Msg::Complete { .. } | Msg::Error { .. }),
            ) {
                waiting.remove(&sub);
            }
            self.apply(msg);
        }
    }

    /// Drains poll messages until the open page's sampler has reported once - and returns at
    /// once if it already has, for the reason `settle_stats_once` is idempotent: a caller that
    /// settled the page's panes and its names first may already hold the cycle, and the next
    /// one is five minutes away.
    pub async fn settle_sample_once(&mut self) {
        if self.sample_seen {
            return;
        }
        self.settle_until(|msg| matches!(msg, Msg::Sample { .. }))
            .await;
    }

    /// Drains poll messages until the stats poller has reported once - and returns at once if
    /// it already has.
    ///
    /// The idempotence is the point: every other settle helper applies every message it
    /// drains, `Msg::Stats` included, so a test that settled a table or a page first may
    /// already hold the cycle. Without this it would wait `PERIOD` - thirty seconds - for the
    /// next one and time out, and which of the two happened would depend on how the requests
    /// interleaved.
    pub async fn settle_stats_once(&mut self) {
        if self.stats_seen {
            return;
        }
        self.settle_until(|msg| matches!(msg, Msg::Stats { .. }))
            .await;
    }

    /// Drains poll messages until the name warm-up has reported once - and returns at once if
    /// it already has, for the reason `settle_stats_once` is idempotent: every other settle
    /// helper applies every message it drains, `Msg::Names` included, so a caller that settled
    /// a table first may already hold the cycle, and without this it would wait `WARM_PERIOD` -
    /// five minutes - for the next one.
    pub async fn settle_names_once(&mut self) {
        if self.names_seen {
            return;
        }
        self.settle_until(|msg| matches!(msg, Msg::Names { .. }))
            .await;
    }

    /// Drains poll messages until the current view's walk has begun and `pages` of its pages
    /// have been applied; `0` returns as soon as the cycle has announced itself.
    ///
    /// `settle_once` waits for the end of a cycle, which is no use to a test that holds the
    /// server open one page at a time: the frame it is about is the one drawn mid-walk.
    pub async fn settle_walk(&mut self, pages: usize) {
        let Some(sub) = self.view().map(|view| view.sub) else {
            return;
        };
        let (mut started, mut seen) = (false, 0);
        while let Some(msg) = self.poll_rx.recv().await {
            let mine = msg.sub() == Some(sub);
            started |= mine && matches!(&msg, Msg::Started { .. });
            seen += usize::from(mine && matches!(&msg, Msg::Page { .. }));
            self.apply(msg);
            // `Started` arrives once, so a caller that waited for it has already taken it off
            // the channel: past zero, pages are the whole of the question.
            let done = if pages == 0 { started } else { seen >= pages };
            if done {
                return;
            }
        }
    }

    /// Drains poll messages until the current table's first `Complete` or `Error`; what
    /// `--snapshot` and tests wait for.
    ///
    /// It never returns on its own if the poll never arrives: `App` holds the sender the
    /// pollers clone, so the channel cannot close while the app is alive and `recv` would wait
    /// forever. Every caller wraps this in a timeout.
    pub async fn settle_once(&mut self) {
        // Which subscription settles, by id rather than by table key: the detail's single
        // subscription shares the key of the table it sits over. An open detail pane is what
        // the next frame shows, so it is the one to wait for; waiting for the table
        // underneath would mean waiting out a whole poll interval for rows already loaded.
        // A payload pane has no subscription of its own, so what settles is the table it was
        // opened over, exactly as if no pane were open.
        let (sub, from_detail) = match self.detail.as_ref().and_then(|d| d.sub) {
            Some(sub) => (sub, true),
            None => match self.view() {
                Some(view) => (view.sub, false),
                None => return,
            },
        };
        while let Some(msg) = self.poll_rx.recv().await {
            let settled = msg.sub() == Some(sub)
                && if from_detail {
                    matches!(&msg, Msg::Entity { .. } | Msg::Error { .. })
                } else {
                    matches!(&msg, Msg::Complete { .. } | Msg::Error { .. })
                };
            self.apply(msg);
            if settled {
                return;
            }
        }
    }

    /// Somebody is there. Clears the idle pause and runs every subscription immediately, so
    /// the pause is invisible except for `⏸ idle` and the falling rate - the waking key is
    /// also delivered as an ordinary key, and there is no "press any key to resume" mode.
    ///
    /// The one clock read left in `App`, and it has to be: a key carries no time, and neither
    /// does an action landing. What is *measured* from the stamp is [`App::tick`]'s decision,
    /// and that reads the clock it is handed.
    pub(crate) fn woke(&mut self) {
        self.last_input = std::time::Instant::now();
        if let Some(live) = self.live.as_ref()
            && live.scheduler.is_idle()
        {
            for s in live.schedulers() {
                s.set_idle(false);
                s.refresh_all();
            }
            self.dirty = true;
        }
    }

    /// The second. `now` is what the frame dates itself by; `mono` is the monotonic read the
    /// idle pause is measured on, handed in rather than taken so that entering the pause is
    /// asserted without waiting five minutes for it.
    pub fn tick(&mut self, now: SystemTime, mono: std::time::Instant) {
        self.now = now;
        // The other direction, on the second this already runs on: `woke` is what ends the
        // pause, and this is what starts it.
        if let Some(live) = self.live.as_ref() {
            let idle = nutsh_core::scheduler::idle_state(self.last_input, mono);
            if idle != live.scheduler.is_idle() {
                for s in live.schedulers() {
                    s.set_idle(idle);
                }
                self.dirty = true;
            }
        }
        // A pane redrawn taller has to fetch the rows it can now show, and a tick is where a
        // resize has settled by.
        self.resize_panes();
        // Sixty seconds, and only when something changed. A `kill -9` loses at most a minute,
        // which is the right trade for never blocking a frame.
        if self.cache_dirty
            && nutsh_core::cache::now_secs().saturating_sub(self.cache_written) >= CACHE_PERIOD
        {
            let _ = self.write_cache_now();
        }
        self.dirty = true;
    }

    /// Re-subscribe the open page's panes at the budget the last frame's heights ask for.
    pub(super) fn resize_panes(&mut self) {
        let Some(Live {
            stack, scheduler, ..
        }) = self.live.as_mut()
        else {
            return;
        };
        let Some(View::Page(page)) = stack.last_mut() else {
            return;
        };
        page.resize(scheduler);
    }
}
