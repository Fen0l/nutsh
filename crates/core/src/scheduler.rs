//! One tokio task per subscription pushes generation-tagged messages into a channel; the UI
//! drains them into the store. Cycles never overlap: the sleep starts after `Complete`.
//!
//! Channel contract: the receiver drains the channel every loop iteration. A full channel
//! parks the pollers on purpose - dropping a `Page` would silently truncate a generation, and
//! a table would then show fewer rows than the server holds with nothing saying so. A capacity
//! of 64 is a reasonable one: it holds several cycles' worth of pages for the handful of
//! subscriptions a screen opens, so a poller only ever waits for a receiver that has stopped
//! draining altogether. When the receiver is gone for good, every send fails and each task
//! returns rather than fetching for nobody.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use nutsh_prism::{Entity, ListOptions, PrismError};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use crate::actions::{Outcome, PlanRef};
use crate::journal::JournalId;
use crate::source::Source;
use crate::store::{Failure, TableKey, Update};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubId(pub u64);

#[derive(Debug, Clone)]
pub struct Subscription {
    pub key: TableKey,
    /// The wait between cycles, floored at one second: a kind with no `poll_secs`, or a caller
    /// that passes zero, must not spin the poller against the API.
    pub interval: Duration,
    /// `Some(ext_id)`: fetch one entity with `get_in` instead of listing.
    ///
    /// A single subscription may share its `TableKey` with the list beside it; the receiver
    /// applies only its `Entity` messages to the store and shows its `Error` in the detail
    /// view, so that a missing entity never marks the whole table as failed.
    pub single: Option<String>,
    /// Entities per page, 1..=100.
    pub page_size: u32,
    /// An OData `$filter`, verbatim from a page's pane definition; the client drops it when
    /// the kind's `ListParams` says it is not accepted.
    pub filter: Option<String>,
    /// An OData `$orderby`, likewise. `None` sends the kind's curated `Kind::orderby`, so a
    /// pane that names its own order keeps it and every other list gets the catalog's.
    pub orderby: Option<String>,
    /// The rows one cycle fetches at most: the walk stops at the first page boundary at or
    /// past this, whatever the server's total, and the header's `shown/total` shows the
    /// truncation. `None` walks the whole collection.
    ///
    /// The boundary is a page, not a row, because paging is by `$page`: the offset of a page
    /// is `page * $limit`, so `$limit` cannot be trimmed mid-walk without moving the rows
    /// under it.
    ///
    /// Seeded from `Kind::max_rows` by the constructors that build a list walk. Every kind in
    /// the generated catalog carries one - its curated budget, else
    /// `catalog::DEFAULT_MAX_ROWS`, which the generator writes in - so a list built from the
    /// catalog is always bounded, and no default is applied here: a `Kind` that said `None`
    /// while this silently capped it would be a catalog that lies about its own kinds.
    ///
    /// Unlike `orderby`, `None` here means *no* budget, which is what a caller that must have
    /// every row asks for and what the single-entity constructors have nothing to use.
    pub max_rows: Option<u32>,
    /// Run exactly one cycle, send [`Msg::Done`], and return. The receiver reaps the entry when
    /// it drains that message, which is after everything the cycle sent; see [`Msg::Done`].
    pub once: bool,
    /// Whether the idle pause stops this subscription. `true` for every list, pane and detail;
    /// `false` for a task watch, which is the user's own pending action, and for a `once`,
    /// which has one cycle to run and then returns.
    ///
    /// A task watch is a [`Subscription::single`] like any other, so no constructor can tell
    /// the two apart: [`Subscription::watching`] is what clears the flag, and
    /// `TaskIndex::watch` is its one caller.
    pub pausable: bool,
}

impl Subscription {
    /// Every entity of the table, a full page at a time, up to the kind's row budget: what a
    /// list view opens. The budget is the kind's, never a default invented here; see
    /// [`Subscription::max_rows`].
    pub fn list(key: TableKey, interval: Duration) -> Subscription {
        let max_rows = key.kind.max_rows;
        Subscription {
            key,
            interval,
            single: None,
            page_size: 100,
            filter: None,
            orderby: None,
            max_rows,
            once: false,
            pausable: true,
        }
    }

    /// One entity, refreshed on the same rhythm: what a detail view opens.
    pub fn single(key: TableKey, interval: Duration, ext_id: String) -> Subscription {
        Subscription {
            key,
            interval,
            single: Some(ext_id),
            page_size: 100,
            filter: None,
            orderby: None,
            // One `get_in`, no walk: nothing for a budget to stop.
            max_rows: None,
            once: false,
            pausable: true,
        }
    }

    /// A page pane: a list with the pane's own query.
    pub fn pane(
        key: TableKey,
        interval: Duration,
        filter: Option<String>,
        orderby: Option<String>,
    ) -> Subscription {
        let max_rows = key.kind.max_rows;
        Subscription {
            key,
            interval,
            single: None,
            page_size: 100,
            filter,
            orderby,
            max_rows,
            once: false,
            pausable: true,
        }
    }

    /// This subscription with a pane's budget for a pane drawn `rows` tall.
    pub fn sized(mut self, rows: usize) -> Subscription {
        let (page_size, max_rows) = pane_budget(rows);
        self.page_size = page_size;
        self.max_rows = Some(max_rows);
        self
    }

    /// This subscription following the user's own pending mutation: the idle pause leaves it
    /// alone. A watch is a [`Subscription::single`] over the Task kind and nothing in its shape
    /// says so, which is why this is a modifier rather than a sixth constructor.
    pub fn watching(mut self) -> Subscription {
        self.pausable = false;
        self
    }

    /// One entity, once: the post-action refresh, so a row changes before the next poll cycle.
    pub fn once(key: TableKey, ext_id: String) -> Subscription {
        Subscription {
            key,
            interval: MIN_INTERVAL,
            single: Some(ext_id),
            page_size: 100,
            filter: None,
            orderby: None,
            max_rows: None,
            once: true,
            pausable: false,
        }
    }

    /// One full list, once: what the form's reference picker opens over a kind the store has
    /// not loaded.
    pub fn once_list(key: TableKey) -> Subscription {
        let max_rows = key.kind.max_rows;
        Subscription {
            key,
            interval: MIN_INTERVAL,
            single: None,
            page_size: 100,
            filter: None,
            orderby: None,
            max_rows,
            once: true,
            pausable: false,
        }
    }
}

/// Rows a pane fetches beyond the ones it draws, so a row appearing at the top does not empty
/// the bottom of the pane before the next cycle.
const PANE_HEADROOM: u32 = 10;
/// The smallest budget a pane asks for. Below this the request is the cost, not the rows.
const PANE_MIN_ROWS: u32 = 20;
/// The `$limit` cap: the client's own constant rather than a copy of it, so there is one
/// spelling of it in reach of this file and a drift is impossible rather than merely tested
/// for. [`pane_budget`] stays pure - this is a `const`, not a client.
const PAGE_LIMIT: u32 = nutsh_prism::client::PAGE_LIMIT_MAX;

/// `(page_size, max_rows)` for a pane drawn `rows` tall. `$limit` is capped at 100 either way,
/// so a very tall pane pages; the budget is what stops the walk.
///
/// This is correct only because a pane's order is deterministic - the curated `Kind::orderby`
/// and the pane's own `orderby` guarantee it, which is why [`Subscription::pane`] already
/// carries one. `shown/total` stays honest because `total` is still the server's, off page 0.
pub fn pane_budget(rows: usize) -> (u32, u32) {
    let needed = u32::try_from(rows)
        .unwrap_or(u32::MAX)
        .saturating_add(PANE_HEADROOM)
        .max(PANE_MIN_ROWS);
    (needed.min(PAGE_LIMIT), needed)
}

/// Input silence before every pausable subscription stops.
pub const IDLE_AFTER: Duration = Duration::from_secs(300);

/// Whether the session is idle: five minutes with no key, resize, action or context switch. A
/// pure function so the rule is asserted without a terminal or a clock.
pub fn idle_state(last_input: std::time::Instant, now: std::time::Instant) -> bool {
    now.saturating_duration_since(last_input) >= IDLE_AFTER
}

/// Tells one `Scheduler` from the next. Never reset, so no two schedulers of a process share
/// one, and a message that outlived a context switch can be recognised as the old one's.
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The floor for `Subscription::interval`. Public because `refresh::parse` refuses a smaller
/// number rather than clamping it, and the floor is cited there rather than copied: the
/// parser's refusal and the poller's clamp are the same number.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Pages one cycle walks at most. An interactive table never needs more than 20 000 rows; the
/// walk returns `Ok(())` at the cap, so the partial list still completes and the header's
/// `rows`/`total` shows the truncation instead of the screen hanging on a paging loop.
const MAX_PAGES: u32 = 200;

/// Consecutive skips before a cycle walks whatever the probe says.
pub(crate) const MAX_SKIPS: u32 = 9;

/// What the probe knows about one table between cycles. **Never derived from `rows`:** the rows
/// are in the table's own `orderby` - creation order for both armed kinds - while `probe_by`
/// is last-modified, so the two name different rows almost always and a comparison against
/// `rows[0]` would mismatch on its first cycle and disarm by construction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ProbeState {
    /// Not calibrated yet: the next cycle probes *and* walks.
    #[default]
    Unarmed,
    /// This Prism Central did not honour `$orderby` on `probe_by`, or reports no total. Never
    /// probe this table again this session.
    Disarmed,
    Armed {
        sort_value: String,
        ext_id: String,
        total: u64,
        skips: u32,
    },
}

/// One `$limit=1` answer: the envelope's total and one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeAnswer {
    pub sort_value: Option<String>,
    /// `None` when the collection was empty. Distinct from a row that carries no `probe_by`
    /// value: an empty table teaches calibration nothing, where a row without the field
    /// proves the probe can never decide.
    pub ext_id: Option<String>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeDecision {
    Skip,
    Walk,
}

/// Skip the walk only when the newest row by last-modified is the same row with the same
/// timestamp *and* the count is unchanged. A stronger claim than "the first row of page 0 is
/// unchanged", for the same one request.
pub(crate) fn probe_decision(
    state: &ProbeState,
    answer: &ProbeAnswer,
    dirty: bool,
) -> ProbeDecision {
    if dirty {
        return ProbeDecision::Walk;
    }
    let ProbeState::Armed {
        sort_value,
        ext_id,
        total,
        skips,
    } = state
    else {
        return ProbeDecision::Walk;
    };
    if *skips >= MAX_SKIPS {
        return ProbeDecision::Walk;
    }
    let (Some(seen), Some(seen_id), Some(now_total)) =
        (&answer.sort_value, &answer.ext_id, answer.total)
    else {
        return ProbeDecision::Walk;
    };
    if seen == sort_value && seen_id == ext_id && now_total == *total {
        ProbeDecision::Skip
    } else {
        ProbeDecision::Walk
    }
}

/// What a cycle that probed **and** walked leaves behind.
///
/// A probe is only sound if the Prism Central honours `$orderby` on `probe_by`: if the probe's
/// row carries a value at least as recent as the maximum across every row the walk returned,
/// the PC sorted. `walk` is `Some` on the one cycle that asks the question - the first
/// eligible one, which walks *before* it probes so the answer is at least as recent as
/// anything the walk saw - and `None` afterwards, when the answer is only refreshing the
/// baseline of a table this Prism Central has already sorted correctly once.
///
/// Both sides are **parsed** rather than compared as bytes. Prism trims trailing zeros from
/// fractional seconds - `crates/mockpc/fixtures-lab/prism/v4.4/config/tasks.json` holds
/// `2026-09-05T17:36:01.21951Z` beside `2026-09-05T17:36:01.220153Z` - so the values are of
/// variable length, and one whose fraction is a prefix of another's compares *greater* by byte
/// order while being earlier in time, `Z` being 0x5A and a digit at most 0x39. A comparison
/// that got that backwards would disarm a Prism Central that sorted, for the session.
///
/// Note what this does *not* prove: an earlier draft claimed it validated `total` honesty, and
/// it cannot - both totals are the same `totalAvailableResults` read from the same page 0.
pub(crate) fn calibrate(
    state: &ProbeState,
    answer: &ProbeAnswer,
    walk: Option<&Walk>,
) -> ProbeState {
    if *state == ProbeState::Disarmed {
        return ProbeState::Disarmed;
    }
    if answer.ext_id.is_none() && answer.sort_value.is_none() {
        // The collection was empty when the probe asked. Nothing was proven either way, and a
        // table that merely happens to hold no rows for one cycle must not lose its probe for
        // the session.
        return state.clone();
    }
    let (Some(sort_value), Some(ext_id), Some(total)) = (
        answer.sort_value.clone(),
        answer.ext_id.clone(),
        answer.total,
    ) else {
        // No total to compare, or a row that does not carry the field: nothing the probe could
        // ever decide on.
        return ProbeState::Disarmed;
    };
    let probed = instant_of(&sort_value);
    // The calibrating cycle **requires** the comparison it exists to make. A probe answer this
    // program cannot parse, and a walk that returned rows without one readable `probe_by` value
    // among them, are both a Prism Central answering something other than the RFC 3339
    // timestamp its own spec declares - and arming a table whose `$orderby` was never validated
    // is the one direction this check exists to prevent. A walk over an empty collection proves
    // nothing either way and is not held against it.
    if *state == ProbeState::Unarmed
        && (probed.is_none() || walk.is_some_and(|w| w.max_sort.is_none() && w.saw_rows))
    {
        return ProbeState::Disarmed;
    }
    if let Some(max) = walk.and_then(|w| w.max_sort.as_deref()) {
        match (probed, instant_of(max)) {
            // A value that will not parse is a value the probe cannot compare, whichever side
            // it is on.
            (Some(probed), Some(newest)) if probed >= newest => {}
            _ => return ProbeState::Disarmed,
        }
    }
    ProbeState::Armed {
        sort_value,
        ext_id,
        total,
        skips: 0,
    }
}

/// One RFC 3339 timestamp as an instant, or `None` when the value is not one.
fn instant_of(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

/// The probe's state for one subscription, shared between the `Scheduler`'s entry and the task
/// that polls. It lives here rather than on `store::Table` because a `Table` is inside `Store`
/// on the UI thread and this task holds no reference to one: the only reader and the only
/// writer of a probe baseline are both here.
#[derive(Debug, Default)]
pub(crate) struct ProbeCell {
    state: Mutex<ProbeState>,
    /// The next cycle must walk, whatever the probe would say.
    dirty: AtomicBool,
    /// Pages the last completed walk took. Below two, probing costs what walking costs.
    pages: AtomicU32,
}

impl ProbeCell {
    fn state(&self) -> ProbeState {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, state: ProbeState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
    }

    fn pages(&self) -> u32 {
        self.pages.load(Ordering::Relaxed)
    }

    fn set_pages(&self, pages: u32) {
        self.pages.store(pages, Ordering::Relaxed);
    }

    pub(crate) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    /// A skip: the baseline stands, and one more skip is on the clock.
    fn skipped(&self) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let ProbeState::Armed { skips, .. } = &mut *guard {
            *skips += 1;
        }
    }
}

/// What a subscription reports. Every message names its subscription and table so the
/// receiver can route it, and its generation so stale cycles are dropped.
#[derive(Debug, Clone)]
pub enum Msg {
    /// A listing cycle has begun, before its first request. See [`Update::Started`].
    Started {
        sub: SubId,
        key: TableKey,
        generation: u64,
    },
    Page {
        sub: SubId,
        key: TableKey,
        generation: u64,
        entities: Vec<Entity>,
        total: Option<u64>,
    },
    Complete {
        sub: SubId,
        key: TableKey,
        generation: u64,
    },
    Entity {
        sub: SubId,
        key: TableKey,
        generation: u64,
        entity: Entity,
    },
    Error {
        sub: SubId,
        key: TableKey,
        generation: u64,
        /// Typed, not a sentence: `Failure::Cause::NotServed` is the 404 on a **list** path
        /// that means this Prism Central does not have the endpoint, whatever the spec
        /// declares - the same answer every cycle, so the subscription stops - and the screen
        /// chooses its own words from it.
        error: Failure,
    },
    /// The Disaster Recovery page's bounded sampler. Not about a table, so it carries neither
    /// a `SubId` nor a `TableKey`; `generation` is the session's, so a cycle that outlived a
    /// context switch is dropped rather than landing in the new Prism Central's store.
    Sample {
        generation: u64,
        sample: crate::sampler::ProtectionSample,
    },
    /// The header's counter poller, on the same terms as `Sample`: not about a table, and
    /// tagged with the session's generation so a cycle that outlived a context switch never
    /// drops the previous Prism Central's counters into the new store.
    Stats {
        generation: u64,
        stats: crate::stats::Stats,
    },
    /// The name cache's warm-up, on the same terms as `Sample` and `Stats`: not about a table,
    /// and tagged with the session's generation so a cycle that outlived a context switch never
    /// drops the previous Prism Central's names into the new store.
    Names {
        generation: u64,
        /// extId → display name, in the order the cycle read them.
        names: Vec<(String, String)>,
    },
    /// A `once` subscription has run its cycle and returned; the receiver reaps it with
    /// [`Scheduler::reap`].
    ///
    /// Reaping on the *receiver* side is the whole point. The channel buffers 64 messages and
    /// `App::apply` drops anything failing [`Scheduler::is_live`], so an entry removed on the
    /// sender side - by the task itself, or by a sweep some other `subscribe` triggered -
    /// races the drain and can gate out the very `Entity` a `once` exists to deliver. This
    /// message is sent last, so by the time it is drained there is nothing left to lose, and
    /// `tasks` still cannot grow: every `once` is removed exactly once, by the receiver that
    /// saw its last message.
    ///
    /// `epoch` names the scheduler that sent it, because this is the one message that *removes*
    /// a subscription: the receiver keeps one poll channel across a context switch while
    /// subscription ids restart with the new scheduler, so a `Done` drained after the switch
    /// would otherwise silently stop whichever view of the new session took that id.
    Done { epoch: u64, sub: SubId },
    /// The result of a mutation, on the same channel as the polls. A third `mpsc` was rejected:
    /// `run`'s `select!` would grow a fifth arm and a *third* place that folds asynchronous
    /// news into the model, while the task watch that follows every action is *already* a
    /// `Subscription` delivering `Msg::Entity` here.
    Acted {
        journal: JournalId,
        plan: PlanRef,
        result: Result<Outcome, String>,
    },
}

impl Msg {
    /// The subscription this message is about, or `None` for one that is not about a table.
    pub fn sub(&self) -> Option<SubId> {
        match self {
            Msg::Started { sub, .. }
            | Msg::Page { sub, .. }
            | Msg::Complete { sub, .. }
            | Msg::Entity { sub, .. }
            | Msg::Error { sub, .. }
            | Msg::Done { sub, .. } => Some(*sub),
            Msg::Sample { .. } | Msg::Stats { .. } | Msg::Names { .. } | Msg::Acted { .. } => None,
        }
    }

    /// The store update this message carries, with its table key; `None` for one that is not
    /// about a table.
    pub fn into_update(self) -> Option<(TableKey, Update)> {
        match self {
            Msg::Started {
                key, generation, ..
            } => Some((key, Update::Started { generation })),
            Msg::Page {
                key,
                generation,
                entities,
                total,
                ..
            } => Some((
                key,
                Update::Page {
                    generation,
                    entities,
                    total,
                },
            )),
            Msg::Complete {
                key, generation, ..
            } => Some((key, Update::Complete { generation })),
            Msg::Entity {
                key,
                generation,
                entity,
                ..
            } => Some((key, Update::Entity { generation, entity })),
            Msg::Error {
                key,
                generation,
                error,
                ..
            } => Some((key, Update::Error { generation, error })),
            Msg::Sample { .. }
            | Msg::Stats { .. }
            | Msg::Names { .. }
            | Msg::Acted { .. }
            | Msg::Done { .. } => None,
        }
    }
}

/// What a `Scheduler` entry and its polling task both hold: the `Notify` the entry rings to
/// run a cycle now, and the probe's state between cycles. One `Arc` rather than two, so the
/// pair cannot drift apart and `run` takes a handle instead of a longer parameter list.
///
/// No `Default`, deliberately: a `Controls` built without its `Scheduler`'s flag would carry a
/// private one no [`Scheduler::set_idle`] can reach, and its task would then never pause -
/// silently, and only for the one subscription that got it.
#[derive(Debug)]
struct Controls {
    wake: Notify,
    probe: ProbeCell,
    /// The session's idle flag, one `Arc` shared by every task of one [`Scheduler`]. It rides
    /// here rather than in [`run`]'s parameters because this is already the handle a task
    /// consults between cycles, and one flag in one place is what lets [`Scheduler::set_idle`]
    /// reach every poller at once without a message.
    idle: Arc<AtomicBool>,
    /// Set by [`Scheduler::stop`] and cleared by [`Scheduler::refresh`]: the paging loop breaks
    /// after the page it is on, and the cycle loop then waits for a wake instead of the
    /// interval. It rides here rather than beside the `JoinHandle` for the reason `idle` does -
    /// this is the one `Arc` the entry and its task already share, so the pair cannot drift.
    stop: AtomicBool,
    /// Seconds between cycles, or **0 for no schedule at all**. Written by
    /// [`Scheduler::set_interval`], read where the sleep would be - so a schedule changed while
    /// this task is asleep takes effect on the wake rather than one cycle later.
    interval: AtomicU32,
    /// Whether the next wake means "run a cycle now". Every waker sets it; a bare schedule
    /// change does not, which is what keeps `off` from costing one last cycle nobody asked for.
    armed: AtomicBool,
}

struct Running {
    handle: JoinHandle<()>,
    controls: Arc<Controls>,
    kind: &'static str,
    /// What the task was spawned from, kept so [`Scheduler::refresh`] can spawn it again. A
    /// task that stopped on a not-served 404 has no `Notify` left to wake, and the person
    /// pressing `^r` over that view is exactly the "somebody watching" the stop was justified
    /// by.
    sub: Subscription,
    /// Copied off the subscription: what [`Scheduler::set_interval_kind`] retimes and what the
    /// idle pause stops. One flag, two features, no second list of exempt pollers.
    pausable: bool,
}

pub struct Scheduler {
    client: Arc<dyn Source>,
    tx: mpsc::Sender<Msg>,
    tasks: HashMap<SubId, Running>,
    next_id: u64,
    /// This scheduler's own number; see [`Msg::Done`].
    epoch: u64,
    /// One counter for every task, so generations are monotone per table even when a view is
    /// unsubscribed and subscribed again: the store drops anything older than what it holds.
    generations: Arc<AtomicU64>,
    /// One flag every pausable task consults where its sleep would be.
    idle: Arc<AtomicBool>,
}

impl Scheduler {
    pub fn new(client: Arc<dyn Source>, tx: mpsc::Sender<Msg>) -> Scheduler {
        Scheduler {
            client,
            tx,
            tasks: HashMap::new(),
            next_id: 1,
            epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed),
            generations: Arc::new(AtomicU64::new(0)),
            idle: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Spawns the polling task, so a tokio runtime must be entered: the TUI subscribes from
    /// inside its runtime, and tests from `#[tokio::test]`.
    ///
    /// No sweep of finished tasks here, deliberately: a `once` is reaped by the receiver when
    /// it drains that subscription's [`Msg::Done`], never by an unrelated `subscribe` that
    /// happens to run between a cycle's send and the UI's next drain.
    pub fn subscribe(&mut self, sub: Subscription) -> SubId {
        let id = SubId(self.next_id);
        self.next_id += 1;
        // The probe's state belongs to the pair that uses it - this entry, which marks it
        // dirty, and the task, which reads and writes the baseline - and to nothing else. A
        // `Subscription` is plain data a caller builds and hands over, so a cell carried on
        // one would be shared by every clone of it.
        let controls = Arc::new(Controls {
            wake: Notify::new(),
            probe: ProbeCell::default(),
            idle: self.idle.clone(),
            stop: AtomicBool::new(false),
            // Never zero here: 0 means `off` and is only ever written by `set_interval`, so a
            // caller that passes a zero `Duration` must not turn its own subscription off.
            interval: AtomicU32::new(secs_of(sub.interval)),
            armed: AtomicBool::new(false),
        });
        let kind = sub.key.kind.id;
        let pausable = sub.pausable;
        let handle = tokio::spawn(run(
            self.client.clone(),
            self.tx.clone(),
            self.epoch,
            id,
            sub.clone(),
            controls.clone(),
            self.generations.clone(),
        ));
        self.tasks.insert(
            id,
            Running {
                handle,
                controls,
                kind,
                sub,
                pausable,
            },
        );
        id
    }

    /// Aborts the task and forgets the subscription. A message it already sent may still
    /// arrive; receivers ignore dead ids. Aborting a task that has already returned - which is
    /// what draining a `once`'s [`Msg::Done`] does - is a no-op.
    pub fn unsubscribe(&mut self, id: SubId) {
        if let Some(task) = self.tasks.remove(&id) {
            task.handle.abort();
        }
    }

    /// Forget a `once` whose task has returned, on the receiver's word: the only removal the
    /// receiver drives, and the reason `subscribe` sweeps nothing. A [`Msg::Done`] from a
    /// previous scheduler carries a different `epoch` and is ignored.
    pub fn reap(&mut self, epoch: u64, id: SubId) {
        if epoch == self.epoch {
            self.unsubscribe(id);
        }
    }

    /// Runs a cycle now, without waiting out the interval or the back-off - a failing view
    /// that the user retries by hand must not sit through a doubled wait. A task busy with a
    /// cycle keeps the permit `Notify` stores and runs one extra cycle straight after it.
    ///
    /// A task that has **returned** is spawned again instead: a list that answered 404 stops
    /// polling (see [`run`]), and waking a task that is no longer there would make `^r` over
    /// that view do nothing at all - no request, no state change, nothing on the frame. The
    /// stop is for the screen nobody is watching; a person asking for a retry is not that. The
    /// cycle it runs clears `not_served` if the answer has changed, and stops again if it has
    /// not. A `once` is never respawned: its entry lives only until the receiver drains its
    /// [`Msg::Done`], and a second cycle would send a second one.
    ///
    /// It also **forces a walk**: "refresh" means refresh, and a probe would answer "nothing
    /// changed" to a person who has just said they do not believe that.
    pub fn refresh(&mut self, id: SubId) {
        let (client, tx, epoch, generations) = (
            self.client.clone(),
            self.tx.clone(),
            self.epoch,
            self.generations.clone(),
        );
        let Some(task) = self.tasks.get_mut(&id) else {
            return;
        };
        task.controls.probe.mark_dirty();
        // A wake now has two meanings, and only this one is "run a cycle".
        task.controls.armed.store(true, Ordering::Relaxed);
        // `^r` is the way out of a stopped subscription as well as a request for a cycle now,
        // and the flag is cleared before the branch below so that `^r` over a poller that was
        // stopped *and* has since returned both lifts the pause and spawns the task again.
        task.controls.stop.store(false, Ordering::Relaxed);
        if task.sub.once || !task.handle.is_finished() {
            task.controls.wake.notify_one();
            return;
        }
        task.handle = tokio::spawn(run(
            client,
            tx,
            epoch,
            id,
            task.sub.clone(),
            task.controls.clone(),
            generations,
        ));
    }

    /// End the listing walk after the page it is on, land what staged, and pause the
    /// subscription: no further cycle until [`Scheduler::refresh`], a re-open or a context
    /// switch.
    ///
    /// The staged pages land because the walk returns `Ok(())` and [`run`] sends its
    /// `Complete` as it would for a walk that finished. They are already paid for, and
    /// discarding them would make the keystroke a punishment.
    ///
    /// `false` when there is nothing to stop: the id is not subscribed, or its task has
    /// already returned and no longer reads the flag. Either way nothing changes, and the
    /// caller says so rather than claiming a stop that did not happen.
    pub fn stop(&self, id: SubId) -> bool {
        match self.tasks.get(&id) {
            Some(task) if !task.handle.is_finished() => {
                task.controls.stop.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    /// How often this subscription cycles, or `None` for no schedule at all - `ctrl-t`'s `off`
    /// and `:refresh off`. Clears a `ctrl-x` stop either way: a provisional silence is being
    /// replaced, by a rhythm or by a permanent one.
    ///
    /// A named interval **arms**, so the new rhythm starts with a cycle: you cannot ask for a
    /// rhythm and be given silence. `off` deliberately does not.
    pub fn set_interval(&self, id: SubId, every: Option<Duration>) -> bool {
        let Some(task) = self.tasks.get(&id) else {
            return false;
        };
        task.controls
            .interval
            .store(every.map_or(0, secs_of), Ordering::Relaxed);
        task.controls.stop.store(false, Ordering::Relaxed);
        if every.is_some() {
            task.controls.probe.mark_dirty();
            task.controls.armed.store(true, Ordering::Relaxed);
        }
        task.controls.wake.notify_one();
        true
    }

    /// The same, for every **pausable** subscription over `kind`: what `ctrl-t` on a view asks
    /// for. Returns how many it reached, which is what a test asserts against.
    ///
    /// `pausable` and not a list of exempt kinds: a task watch is the user's own pending
    /// mutation and a `once` has one cycle to run, and those are exactly the two the idle pause
    /// exempts. One flag, two features, no second list to keep in step.
    pub fn set_interval_kind(&self, kind: &str, every: Option<Duration>) -> usize {
        let ids: Vec<SubId> = self
            .tasks
            .iter()
            .filter(|(_, t)| t.kind == kind && t.pausable)
            .map(|(id, _)| *id)
            .collect();
        ids.iter()
            .filter(|id| self.set_interval(**id, every))
            .count()
    }

    /// Whether this subscription has no schedule. What `⏹ manual` is drawn from, and what
    /// [`Scheduler::refresh_all`] skips.
    pub fn is_manual(&self, id: SubId) -> bool {
        self.tasks
            .get(&id)
            .is_some_and(|t| t.controls.interval.load(Ordering::Relaxed) == 0)
    }

    /// The same, for every subscription over `kind`: what a completed mutation asks for. The
    /// user just changed something and is watching for it.
    pub fn refresh_kind(&mut self, kind: &str) {
        let ids: Vec<SubId> = self
            .tasks
            .iter()
            .filter(|(_, t)| t.kind == kind)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.refresh(id);
        }
    }

    /// Stop, or restart, every pausable subscription. A task already inside a cycle finishes
    /// it: the flag is consulted where the sleep would be, at both ends of it.
    pub fn set_idle(&self, idle: bool) {
        self.idle.store(idle, Ordering::Relaxed);
    }

    /// Whether the pause is currently holding - what `ui::sync_indicator` and `App::woke` both
    /// ask.
    pub fn is_idle(&self) -> bool {
        self.idle.load(Ordering::Relaxed)
    }

    /// The flag itself, for the pollers that are neither a `Subscription` nor exempt and so
    /// have no task of this scheduler's to consult it: the Disaster Recovery page's sampler and
    /// the name cache's warm-up, each a `tokio::spawn` its owner holds the handle to.
    ///
    /// The two exemptions are the stats poller, whose counters are what a person reads from
    /// across the room, and a task watch, which is a `Subscription` with `pausable` cleared.
    pub fn idle_flag(&self) -> Arc<AtomicBool> {
        self.idle.clone()
    }

    /// Every subscription runs a cycle now, and walks: waking from the pause has the same
    /// unbounded-gap problem a cache restore does, so no probe's baseline survives it.
    ///
    /// `&self`, so a task stopped by a not-served 404 is left stopped rather than respawned -
    /// unlike [`Scheduler::refresh`], which is a person asking for that view by hand.
    pub fn refresh_all(&self) {
        for task in self
            .tasks
            .values()
            // A subscription with no schedule is skipped: this is the idle pause's wake, and
            // without the skip every keystroke after five idle minutes would refresh a view the
            // user turned off.
            .filter(|t| t.controls.interval.load(Ordering::Relaxed) != 0)
        {
            task.controls.probe.mark_dirty();
            task.controls.armed.store(true, Ordering::Relaxed);
            task.controls.wake.notify_one();
        }
    }

    /// Every subscription this session holds runs a cycle now, stopped ones included.
    ///
    /// The person's version of [`Scheduler::refresh_all`]: that one is the idle pause's wake and
    /// leaves a stopped poller stopped, which is right for a wake and wrong for somebody who has
    /// just asked for everything. Each is [`Scheduler::refresh`], so the stop flag is cleared and
    /// the walk is forced, one subscription at a time.
    pub fn refresh_everything(&mut self) -> usize {
        let ids: Vec<SubId> = self.tasks.keys().copied().collect();
        for id in &ids {
            self.refresh(*id);
        }
        ids.len()
    }

    /// Whether this id is still subscribed. It answers for the subscription, not the task: a
    /// `once` whose task has already returned still reads live until the receiver drains its
    /// [`Msg::Done`] and unsubscribes it, and that message is sent after every other, so no
    /// queued message is ever gated out. The receiver uses it to drop messages that a view
    /// already unsubscribed from sent.
    pub fn is_live(&self, id: SubId) -> bool {
        self.tasks.contains_key(&id)
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        for task in self.tasks.values() {
            task.handle.abort();
        }
    }
}

async fn run(
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
        // A list 404 is not the endpoint's absence yet. A restored pin routes a namespace at a
        // version this Prism Central may not serve, and `Client::repair_after` re-negotiates
        // that namespace inside the very call that failed - so the request *behind* a 404 is
        // the one that routes to the version that answers, and stopping on the first would
        // strand a kind the PC serves perfectly well (the same holds for an endpoint that was
        // 5xx and has come back). Every list 404 therefore gets one immediate second attempt,
        // inside the same cycle so the view sees one answer rather than a raw error that
        // becomes a sentence, and only when that answers 404 too is it the endpoint's own.
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
        // And the same for a credential this Prism Central refused, on a sharper reason than
        // the one above. A 404 retried for ever wastes requests; a password retried for ever
        // is failures counted against a real account. `Client::send` already refuses to put
        // one on the wire, so this loop would spin against a gate rather than against the
        // Prism Central - but a task that has nothing left to do should stop, and the table
        // keeps its rows and draws `Failure::text` either way.
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

/// Waits until this subscription's next cycle: `wait`, or - while the session is idle and this
/// subscription pauses - until something wakes it. Nobody is reading it, so it waits on the
/// `Notify` it already holds rather than sleeping the interval, which costs no task, channel or
/// message.
///
/// `pausable` rather than the whole [`Subscription`]: the wait consults that one flag and
/// nothing else of it - not `interval`, which arrives already backed off as `wait`, and not
/// `once`, which never reaches here.
///
/// The flag is read at **both** ends of the sleep. The pause is set from `App::tick`, one
/// second at a time, so on any interval longer than that a task is almost always already asleep
/// when it fires; a task committed to its interval must not buy one more cycle with it.
async fn wait_for_cycle(pausable: bool, controls: &Controls, first: Duration) {
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

/// A 404 on a **top-level list** path: the path is the whole of what was asked for, so the
/// answer is about this Prism Central and not about a row.
///
/// A 404 on a single entity is one row that has gone. A 404 on a *child* list carries its
/// parents' ext ids in the path (`TableKey::under`), so a parent deleted from another client
/// answers exactly this - that is the parent's absence, not the endpoint's, and calling it
/// "not served by this Prism Central" would be a claim about the server for what is one gone
/// row. Both are ordinary errors, retried like any other.
fn is_missing_list(sub: &Subscription, error: Option<&PrismError>) -> bool {
    is_endpoint_list(sub) && matches!(error, Some(PrismError::NotFound(_)))
}

/// A subscription over the kind's **own** list path, with no parent ids in it and no single
/// entity: the one whose answer is about the endpoint rather than about a row. Both the 404
/// that is remembered and the completion that forgets it are judged on it, so they cannot
/// come to disagree about which subscription speaks for the endpoint.
fn is_endpoint_list(sub: &Subscription) -> bool {
    sub.single.is_none() && sub.key.parents.is_empty()
}

/// One cycle's requests and the answer they settled on: the generation it ran under, and the
/// error if it failed. `None` is the receiver having gone away, which is the only reason a task
/// stops in the middle of a cycle rather than at the end of one.
async fn attempt(
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
fn secs_of(interval: Duration) -> u32 {
    u32::try_from(poll_interval(interval).as_secs()).unwrap_or(u32::MAX)
}

/// The interval a cycle actually waits.
fn poll_interval(interval: Duration) -> Duration {
    interval.max(MIN_INTERVAL)
}

/// How long to wait before the next cycle, and the back-off to carry into it. A success
/// resets both to the interval. A failure waits the back-off it arrived with - so the first
/// failure waits exactly the interval - and doubles it for the next one, never below the
/// interval and never above a minute. `RateLimited` waits what the server asked when that is
/// longer than the back-off.
fn next_backoff(
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

/// Whether the walk fetches another page after this one. A kind that does not page, an empty
/// page, the row budget, and the page cap all end it; then a known total decides, and without
/// one only a full page suggests there is more.
///
/// `budget` is the subscription's `max_rows`: a walk that has reached it stops there, however
/// many rows the server holds, so a table over a collection a Prism Central never trims costs
/// a fixed handful of pages a cycle instead of the whole collection.
fn more_pages(
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
    pages: u32,
    /// The largest `probe_by` value across every row of the walk, for calibration. `None` when
    /// the walk was not scanned for one, when it returned no row, and when no row it returned
    /// carried a value [`instant_of`] could read - which is why `saw_rows` is beside it.
    max_sort: Option<String>,
    /// Whether the walk returned any row at all. A calibrating walk that read rows and found no
    /// readable value among them has shown this Prism Central answers something the probe can
    /// never compare; one over an empty collection has shown nothing.
    saw_rows: bool,
}

/// One list cycle, with the probe when the kind and the table qualify.
///
/// Three conditions, checked here: the kind carries a curated `probe_by`, this table's probe
/// is not `Disarmed`, and its last walk took **two or more pages**. A one-page table's probe
/// costs what its walk costs, so probing it is pure loss. That the field is a declared
/// timestamp and that the endpoint takes `$orderby` and `$limit` are the generator's checks,
/// made once against the spec in `xtask::catalog::overlay::apply`; whether this Prism Central
/// actually honours the order is [`calibrate`]'s, made once against the server.
async fn cycle(
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
        // The calibrating cycle walks whatever the probe would say - [`probe_decision`] answers
        // `Walk` for every state but `Armed` - so the probe need not come first here, and
        // asking it *after* the last page is what stops the check racing itself: a probe issued
        // first is a request older than the walk beside it, so any entity updated during the
        // walk carries a value newer than the probe's row through no fault of the Prism
        // Central's, and this one cycle is the one that decides the session. Asked last, the
        // answer is at least as recent as anything the walk saw on a PC that sorts; one that
        // ignores `$orderby` still hands back a stale row, and still disarms.
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
    // The sort check is asked once, of an `Unarmed` table, in the branch above. Re-asking it
    // here would disarm on a benign race and not on a fault: this probe *is* a request older
    // than the walk under it, so an entity updated between the two is legitimately newer than
    // the answer the probe holds - and a walk is decided on precisely when the table is
    // changing. Past that first cycle the walk is not scanned for a maximum at all: `None` for
    // `probe_by`, and `None` for the walk `calibrate` is handed.
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
async fn probe(
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
async fn list(
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
async fn fetch_one(
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

#[cfg(test)]
mod tests {
    use super::*;

    const FIVE: Duration = Duration::from_secs(5);

    fn err() -> PrismError {
        PrismError::NotFound("gone".into())
    }

    /// Which 404 is a claim about this Prism Central, and which is a claim about one row.
    #[test]
    fn only_a_top_level_lists_404_is_the_endpoint_missing() {
        let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
        let top = Subscription::list(TableKey::top(vm), FIVE);
        assert!(is_missing_list(&top, Some(&err())));
        assert!(!is_missing_list(&top, None), "a cycle that answered");
        assert!(
            !is_missing_list(&top, Some(&PrismError::Decode("nonsense".into()))),
            "any other failure is the service, not the endpoint"
        );
        // The path carries a parent's ext id, so the 404 is that parent's: drill into a VM's
        // disks and delete the VM from another client, and this is the answer.
        let child = Subscription::list(TableKey::under(vm, vec!["parent".into()]), FIVE);
        assert!(!is_missing_list(&child, Some(&err())));
        // And one entity that has gone.
        let single = Subscription::single(TableKey::top(vm), FIVE, "gone".into());
        assert!(!is_missing_list(&single, Some(&err())));
        // A pane's filtered table is still a top-level list.
        let pane = Subscription::pane(
            TableKey::filtered(vm, Some("powerState eq 'ON'")),
            FIVE,
            None,
            None,
        );
        assert!(is_missing_list(&pane, Some(&err())));
    }

    #[test]
    fn the_first_failure_waits_exactly_the_interval_then_doubles_to_the_cap() {
        let (wait, next) = next_backoff(FIVE, FIVE, Some(&err()));
        assert_eq!((wait, next), (FIVE, Duration::from_secs(10)));
        let steps: Vec<u64> = std::iter::successors(Some(FIVE), |current| {
            Some(next_backoff(FIVE, *current, Some(&err())).1)
        })
        .map(|d| d.as_secs())
        .take(6)
        .collect();
        assert_eq!(steps, [5, 10, 20, 40, 60, 60]);
    }

    #[test]
    fn a_long_interval_is_never_backed_off_below() {
        let hour = Duration::from_secs(3600);
        let (wait, next) = next_backoff(hour, hour, Some(&err()));
        assert_eq!((wait, next), (hour, hour));
    }

    #[test]
    fn rate_limited_waits_what_the_server_asked_when_it_is_longer() {
        let retry_after = Duration::from_secs(90);
        let (wait, next) = next_backoff(FIVE, FIVE, Some(&PrismError::RateLimited { retry_after }));
        assert_eq!(wait, retry_after);
        assert_eq!(next, Duration::from_secs(10));
        // A shorter Retry-After does not undo the back-off already earned.
        let (wait, _) = next_backoff(
            FIVE,
            Duration::from_secs(40),
            Some(&PrismError::RateLimited {
                retry_after: Duration::from_secs(1),
            }),
        );
        assert_eq!(wait, Duration::from_secs(40));
    }

    /// And the same for a 503, which is the answer a busy Prism Central gives where a 429
    /// would be a polite one: "not now" carries how long, and a poller that ignores it asks
    /// again into the same outage.
    #[test]
    fn a_503_waits_the_retry_after_it_sent() {
        let down = |retry_after| PrismError::Api {
            status: 503,
            message: "backend service unavailable".into(),
            retry_after,
        };
        let (wait, next) = next_backoff(FIVE, FIVE, Some(&down(Some(Duration::from_secs(30)))));
        assert_eq!(
            (wait, next),
            (Duration::from_secs(30), Duration::from_secs(10))
        );
        // Without the header it is the ordinary back-off, which is what it always was.
        assert_eq!(
            next_backoff(FIVE, FIVE, Some(&down(None))),
            (FIVE, FIVE * 2)
        );
    }

    #[test]
    fn a_success_resets_to_the_interval() {
        assert_eq!(
            next_backoff(FIVE, Duration::from_secs(60), None),
            (FIVE, FIVE)
        );
    }

    #[test]
    fn a_zero_interval_is_floored_to_a_second() {
        assert_eq!(poll_interval(Duration::ZERO), Duration::from_secs(1));
        assert_eq!(poll_interval(Duration::from_millis(200)), MIN_INTERVAL);
        assert_eq!(poll_interval(FIVE), FIVE);
    }

    #[test]
    fn the_walk_ends_on_a_kind_that_does_not_page_and_on_an_empty_page() {
        assert!(!more_pages(false, 100, 100, 100, None, 0, None));
        assert!(!more_pages(true, 0, 100, 100, None, 1, None));
    }

    #[test]
    fn a_known_total_decides_the_walk_whatever_the_page_size() {
        // Reached: even a full page stops. Short of it: even a short page continues, which is
        // what a server that trims a page below `$limit` needs.
        assert!(!more_pages(true, 100, 100, 100, Some(100), 0, None));
        assert!(more_pages(true, 40, 100, 40, Some(100), 0, None));
    }

    #[test]
    fn no_budget_walks_a_collection_to_its_total() {
        // 6109 tasks, 100 a page: without a budget every page is fetched, to the last short
        // one, which is the cost this budget exists to cut.
        assert!(more_pages(true, 100, 100, 100, Some(6109), 0, None));
        assert!(more_pages(true, 100, 100, 6000, Some(6109), 59, None));
        assert!(!more_pages(true, 9, 100, 6109, Some(6109), 60, None));
    }

    #[test]
    fn the_budget_ends_the_walk_short_of_the_total() {
        // 500 rows of the same 6109: the fifth page reaches the budget and the walk stops
        // there, with the server's total still known so the header can say `500/6109`.
        assert!(more_pages(true, 100, 100, 400, Some(6109), 3, Some(500)));
        assert!(!more_pages(true, 100, 100, 500, Some(6109), 4, Some(500)));
        // The stop is at a page boundary, not a row: `$page` offsets are `page * $limit`, so
        // a budget between two boundaries overshoots to the next one and stops.
        assert!(more_pages(true, 100, 100, 100, Some(6109), 0, Some(150)));
        assert!(!more_pages(true, 100, 100, 200, Some(6109), 1, Some(150)));
        // A budget the collection never reaches changes nothing: the total still ends it.
        assert!(!more_pages(
            true,
            9,
            100,
            6109,
            Some(6109),
            60,
            Some(20_000)
        ));
    }

    #[test]
    fn without_a_total_only_a_full_page_suggests_more() {
        assert!(more_pages(true, 100, 100, 100, None, 0, None));
        assert!(!more_pages(true, 99, 100, 99, None, 0, None));
    }

    #[test]
    fn the_page_cap_ends_the_walk() {
        assert!(more_pages(true, 100, 100, 100, None, MAX_PAGES - 2, None));
        assert!(!more_pages(true, 100, 100, 100, None, MAX_PAGES - 1, None));
    }

    #[test]
    fn a_list_takes_the_budget_from_its_kind_and_a_single_never_has_one() {
        let tasks = TableKey::top(nutsh_catalog::kind("prism.config.Task").expect("Tasks"));
        assert_eq!(
            Subscription::list(tasks.clone(), FIVE).max_rows,
            tasks.kind.max_rows
        );
        assert!(tasks.kind.max_rows.is_some(), "Tasks are curated with one");
        assert_eq!(
            Subscription::once_list(tasks.clone()).max_rows,
            tasks.kind.max_rows
        );
        assert_eq!(
            Subscription::pane(tasks.clone(), FIVE, None, None).max_rows,
            tasks.kind.max_rows
        );
        // Nothing to walk: one `get_in`.
        assert_eq!(
            Subscription::single(tasks.clone(), FIVE, "t".into()).max_rows,
            None
        );
        assert_eq!(Subscription::once(tasks, "t".into()).max_rows, None);
    }

    /// A kind nobody curated a budget for is still bounded: the generator wrote
    /// `DEFAULT_MAX_ROWS` into it, so a list stops ten pages in instead of walking a
    /// collection a Prism Central never trims.
    #[test]
    fn a_kind_with_no_curated_budget_stops_at_the_default() {
        let vms = TableKey::top(nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs"));
        assert_eq!(vms.kind.max_rows, Some(nutsh_catalog::DEFAULT_MAX_ROWS));
        let sub = Subscription::list(vms.clone(), FIVE);
        assert_eq!(sub.max_rows, Some(nutsh_catalog::DEFAULT_MAX_ROWS));
        assert_eq!(
            Subscription::once_list(vms.clone()).max_rows,
            Some(nutsh_catalog::DEFAULT_MAX_ROWS)
        );
        assert_eq!(
            Subscription::pane(vms, FIVE, None, None).max_rows,
            Some(nutsh_catalog::DEFAULT_MAX_ROWS)
        );
        // And that budget is what ends the walk: ten pages of 100 out of a collection of
        // 181 825, with the total still known so the header can say `1000/181825`.
        let budget = sub.max_rows.map(u64::from);
        assert!(more_pages(true, 100, 100, 900, Some(181_825), 8, budget));
        assert!(!more_pages(true, 100, 100, 1000, Some(181_825), 9, budget));
    }

    /// The default is a floor for the uncurated, not a ceiling over the curated: a kind that
    /// names its own budget keeps it, in either direction.
    #[test]
    fn a_curated_budget_still_wins_over_the_default() {
        for id in ["prism.config.Task", "monitoring.serviceability.Alert"] {
            let key = TableKey::top(nutsh_catalog::kind(id).expect(id));
            assert_eq!(key.kind.max_rows, Some(500), "{id} curates 500");
            assert_eq!(Subscription::list(key, FIVE).max_rows, Some(500), "{id}");
        }
        // The audits, the kind this budget is for: 181 825 rows at 0.85 s a page is 1819
        // pages, of which `MAX_PAGES` would fetch 200 in 170 s of every 30 s cycle, for
        // 20 000 rows nobody reads. Five pages and four seconds with the budget.
        let audits =
            TableKey::top(nutsh_catalog::kind("monitoring.serviceability.Audit").expect("Audits"));
        assert_eq!(audits.kind.max_rows, Some(500));
        assert_eq!(audits.kind.orderby, Some("creationTime desc"));
        assert_eq!(Subscription::list(audits, FIVE).max_rows, Some(500));
    }

    /// A pane's budget comes from the height it was drawn at, not from the kind's catalog
    /// budget: the Dashboard's alerts pane walks 500 alert objects every ten seconds to draw
    /// ten rows - 180 000 an hour - and asks for twenty afterwards.
    #[test]
    fn a_pane_budget_is_its_drawn_height_plus_headroom_floored_at_twenty() {
        assert_eq!(pane_budget(10), (20, 20));
        assert_eq!(
            pane_budget(0),
            (20, 20),
            "a pane with no room still asks once"
        );
        assert_eq!(pane_budget(40), (50, 50));
        assert_eq!(pane_budget(95), (100, 105), "$limit is capped at 100");
        assert_eq!(pane_budget(500), (100, 510));
        assert_eq!(
            pane_budget(usize::MAX),
            (100, u32::MAX),
            "a height that does not fit in a u32 saturates rather than wrapping"
        );
    }

    /// A table **view** keeps its catalog budget: the user can scroll it, so its budget is not
    /// its height. Only a pane's is.
    #[test]
    fn a_pane_takes_the_budget_and_a_table_view_keeps_the_catalog_s() {
        let alerts =
            TableKey::top(nutsh_catalog::kind("monitoring.serviceability.Alert").expect("Alerts"));
        let sub = Subscription::pane(alerts.clone(), FIVE, None, None).sized(10);
        assert_eq!((sub.page_size, sub.max_rows), (20, Some(20)));
        assert_eq!(
            Subscription::list(alerts.clone(), FIVE).max_rows,
            alerts.kind.max_rows,
            "a table view is not a pane"
        );
        assert_eq!(alerts.kind.max_rows, Some(500));
    }

    fn answer(sort: &str, id: &str, total: u64) -> ProbeAnswer {
        ProbeAnswer {
            sort_value: Some(sort.into()),
            ext_id: Some(id.into()),
            total: Some(total),
        }
    }

    fn armed(sort: &str, id: &str, total: u64, skips: u32) -> ProbeState {
        ProbeState::Armed {
            sort_value: sort.into(),
            ext_id: id.into(),
            total,
            skips,
        }
    }

    /// The walk beside a calibrating probe: it read rows, and the newest `probe_by` value among
    /// them is `max`. `pages` is what the walk cost and calibration never reads it.
    fn walked(max: &str) -> Walk {
        Walk {
            pages: 4,
            max_sort: Some(max.into()),
            saw_rows: true,
        }
    }

    /// A calibrating walk with no maximum to offer: either it read rows and not one `probe_by`
    /// value among them this program could parse, or it read no row at all.
    fn walked_without_a_maximum(saw_rows: bool) -> Walk {
        Walk {
            pages: 4,
            max_sort: None,
            saw_rows,
        }
    }

    /// All three equal: nothing was created, updated or deleted since the previous probe. A
    /// create or an edit produces a strictly newer `lastUpdatedTime`; a delete moves the total.
    #[test]
    fn a_probe_skips_only_when_all_three_halves_match() {
        let state = armed("2026-09-09T10:00:00Z", "t1", 6109, 0);
        assert_eq!(
            probe_decision(&state, &answer("2026-09-09T10:00:00Z", "t1", 6109), false),
            ProbeDecision::Skip
        );
        for changed in [
            answer("2026-09-09T10:00:01Z", "t1", 6109),
            answer("2026-09-09T10:00:00Z", "t2", 6109),
            answer("2026-09-09T10:00:00Z", "t1", 6110),
        ] {
            assert_eq!(probe_decision(&state, &changed, false), ProbeDecision::Walk);
        }
    }

    /// Four ways a probe cannot be trusted, and each of them walks.
    #[test]
    fn a_probe_walks_when_it_has_nothing_to_stand_on() {
        let state = armed("t", "t1", 10, 0);
        let no_total = ProbeAnswer {
            total: None,
            ..answer("t", "t1", 10)
        };
        assert_eq!(
            probe_decision(&state, &no_total, false),
            ProbeDecision::Walk
        );
        let no_field = ProbeAnswer {
            sort_value: None,
            ..answer("t", "t1", 10)
        };
        assert_eq!(
            probe_decision(&state, &no_field, false),
            ProbeDecision::Walk
        );
        assert_eq!(
            probe_decision(&state, &answer("t", "t1", 10), true),
            ProbeDecision::Walk,
            "dirty"
        );
        assert_eq!(
            probe_decision(&ProbeState::Unarmed, &answer("t", "t1", 10), false),
            ProbeDecision::Walk
        );
        assert_eq!(
            probe_decision(&ProbeState::Disarmed, &answer("t", "t1", 10), false),
            ProbeDecision::Walk
        );
    }

    /// Every tenth cycle walks in full whatever the probe says, so any change the probe
    /// structurally cannot see is bounded at ten cycles: 30 s for tasks, 100 s for alerts. The
    /// cap is not a nicety - it is the only guard covering what calibration and the comparison
    /// both miss, which is why it is a constant and not a setting.
    #[test]
    fn nine_skips_is_the_most_in_a_row() {
        let unchanged = answer("t", "t1", 10);
        for skips in 0..MAX_SKIPS {
            assert_eq!(
                probe_decision(&armed("t", "t1", 10, skips), &unchanged, false),
                ProbeDecision::Skip,
                "{skips}"
            );
        }
        assert_eq!(
            probe_decision(&armed("t", "t1", 10, MAX_SKIPS), &unchanged, false),
            ProbeDecision::Walk
        );
    }

    /// The test the first draft of this design got wrong: a table whose `orderby` and
    /// `probe_by` name **different** fields still arms, because the baseline is the probe's own
    /// answer and never `rows[0]`. Here the walk is ordered by creation and its newest row by
    /// last-modified is a different row entirely.
    #[test]
    fn a_table_ordered_by_creation_still_arms_a_last_modified_probe() {
        let probe = answer("2026-09-09T12:00:00Z", "edited-old-task", 6109);
        let state = calibrate(
            &ProbeState::Unarmed,
            &probe,
            Some(&walked("2026-09-09T12:00:00Z")),
        );
        assert_eq!(
            state,
            armed("2026-09-09T12:00:00Z", "edited-old-task", 6109, 0)
        );
    }

    /// A Prism Central that ignores `$orderby` returns an arbitrary row, which is older than
    /// the newest the walk saw. One extra request, once per table per session, and then the
    /// probe is off for good.
    #[test]
    fn a_pc_that_does_not_sort_disarms_the_probe_for_the_session() {
        let probe = answer("2026-09-09T09:00:00Z", "whatever", 6109);
        let state = calibrate(
            &ProbeState::Unarmed,
            &probe,
            Some(&walked("2026-09-09T12:00:00Z")),
        );
        assert_eq!(state, ProbeState::Disarmed);
        // And a PC that reports no total gives the probe nothing to compare, ever.
        let no_total = ProbeAnswer {
            total: None,
            ..probe
        };
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &no_total,
                Some(&walked("2026-09-09T12:00:00Z"))
            ),
            ProbeState::Disarmed
        );
    }

    /// The one thing calibration is for, and the one way to get it wrong. Prism trims trailing
    /// zeros from fractional seconds, so a value whose fraction is a prefix of another's is
    /// *greater* by byte order and earlier in time. Both directions are asserted, because both
    /// failures are real: the first would arm a Prism Central whose sort was never validated,
    /// the second would disarm one that sorted correctly.
    #[test]
    fn a_trimmed_fraction_is_compared_as_an_instant_and_never_as_bytes() {
        // Chronologically older than the walk's newest, and lexicographically greater.
        let older = answer("2026-09-05T17:36:01.21951Z", "t1", 100);
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &older,
                Some(&walked("2026-09-05T17:36:01.219511Z"))
            ),
            ProbeState::Disarmed
        );
        // And the mirror: newer in time, smaller as bytes.
        let newer = answer("2026-09-05T17:36:01.219511Z", "t1", 100);
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &newer,
                Some(&walked("2026-09-05T17:36:01.21951Z"))
            ),
            armed("2026-09-05T17:36:01.219511Z", "t1", 100, 0)
        );
        // A value neither side can parse is a value the probe cannot compare.
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &answer("not a timestamp", "t1", 100),
                Some(&walked("2026-09-05T17:36:01Z"))
            ),
            ProbeState::Disarmed
        );
    }

    /// Calibration has to compare something, and this is the case that quietly stopped it from
    /// having to. A probe answer this program cannot parse, and a walk that read rows without
    /// one readable `probe_by` value among them, are both a Prism Central answering something
    /// other than the RFC 3339 timestamp its own spec declares; arming on either would arm a
    /// table whose `$orderby` was never validated, which is the one direction the check exists
    /// to prevent. A walk over an empty collection is the exception: it proves nothing either
    /// way, and the probe's own row still arms the table.
    #[test]
    fn a_calibration_with_nothing_to_compare_disarms() {
        let probe = answer("2026-09-09T12:00:00Z", "t1", 10);
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &probe,
                Some(&walked_without_a_maximum(true))
            ),
            ProbeState::Disarmed,
            "rows walked, and not one value among them to compare against"
        );
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &probe,
                Some(&walked_without_a_maximum(false))
            ),
            armed("2026-09-09T12:00:00Z", "t1", 10, 0),
            "an empty collection is not a refusal to sort"
        );
        // Epoch milliseconds: what a Prism Central answering something other than its own
        // declared shape looks like from here.
        let epoch = answer("1757419200000", "t1", 10);
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &epoch,
                Some(&walked("2026-09-09T12:00:00Z"))
            ),
            ProbeState::Disarmed
        );
        assert_eq!(
            calibrate(
                &ProbeState::Unarmed,
                &epoch,
                Some(&walked_without_a_maximum(false))
            ),
            ProbeState::Disarmed,
            "the probe's own value must parse, whatever the walk saw"
        );
        // The asymmetry, stated: an armed table is refreshed and never re-checked, so the same
        // answer only moves its baseline. Past calibration the baseline is compared for
        // equality alone, which needs no clock.
        assert_eq!(
            calibrate(&armed("2026-09-09T11:00:00Z", "t0", 10, 3), &epoch, None),
            armed("1757419200000", "t1", 10, 0)
        );
    }

    /// An armed table is never re-calibrated, and this is why: an armed cycle's probe goes
    /// first - deciding whether the walk happens at all is the point of it - so it is a request
    /// older than the walk beside it, and an entity updated between the two carries a value
    /// newer than the probe's row through no fault of the Prism Central's. A walk is decided on
    /// precisely when the table is changing, so re-checking would disarm the busiest tables
    /// after their first busy cycle. `cycle` hands `calibrate` no walk once armed; the baseline
    /// moves to what the probe saw and the skip count starts again.
    #[test]
    fn an_armed_probe_keeps_its_arming_when_the_walk_saw_a_newer_row() {
        let state = armed("2026-09-09T12:00:00Z", "t1", 6109, 4);
        let fresh = answer("2026-09-09T12:00:05Z", "t2", 6110);
        assert_eq!(
            calibrate(&state, &fresh, None),
            armed("2026-09-09T12:00:05Z", "t2", 6110, 0)
        );
        // The same answer against the same walk, had the check been re-asked, would have
        // disarmed it, which is what this asymmetry avoids.
        assert_eq!(
            calibrate(&state, &fresh, Some(&walked("2026-09-09T12:00:09Z"))),
            ProbeState::Disarmed
        );
    }

    /// A collection that happened to hold no rows teaches calibration nothing: no row is not a
    /// refusal to sort, and a Prism Central whose tasks were purged must not lose its probe for
    /// the session over it.
    #[test]
    fn an_empty_collection_leaves_the_probe_where_it_was() {
        let empty = ProbeAnswer {
            sort_value: None,
            ext_id: None,
            total: Some(0),
        };
        let state = armed("2026-09-09T12:00:00Z", "t1", 6109, 2);
        assert_eq!(calibrate(&state, &empty, None), state);
        assert_eq!(
            calibrate(&ProbeState::Unarmed, &empty, None),
            ProbeState::Unarmed
        );
        // A row that came back without the field is the other thing, and it disarms: the probe
        // could never decide on it.
        let no_field = ProbeAnswer {
            sort_value: None,
            ext_id: Some("t1".into()),
            total: Some(6109),
        };
        assert_eq!(calibrate(&state, &no_field, None), ProbeState::Disarmed);
    }

    /// Five minutes with no key, resize, action or context switch.
    #[test]
    fn idle_starts_at_five_minutes_and_ends_on_any_input() {
        let t0 = std::time::Instant::now();
        assert!(!idle_state(t0, t0));
        assert!(!idle_state(t0, t0 + IDLE_AFTER - Duration::from_secs(1)));
        assert!(idle_state(t0, t0 + IDLE_AFTER));
        assert!(idle_state(t0, t0 + Duration::from_secs(3600)));
    }

    /// Every list, pane and detail pauses; a `once` has one cycle to run and then returns, and
    /// a task watch is the user's own pending action.
    #[test]
    fn a_task_watch_and_a_once_are_the_two_that_never_pause() {
        let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
        let key = TableKey::top(vm);
        assert!(Subscription::list(key.clone(), FIVE).pausable);
        assert!(Subscription::pane(key.clone(), FIVE, None, None).pausable);
        assert!(Subscription::single(key.clone(), FIVE, "v1".into()).pausable);
        assert!(!Subscription::once(key.clone(), "v1".into()).pausable);
        assert!(!Subscription::once_list(key.clone()).pausable);
        // The one both the idle pause and the refresh schedule need `false` on, and the one no
        // constructor can tell apart on its own: a task watch *is* a `single`.
        assert!(
            !Subscription::single(key, FIVE, "v1".into())
                .watching()
                .pausable
        );
    }
}
