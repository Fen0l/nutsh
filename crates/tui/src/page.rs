//! A feature page: several subscriptions composed into panes with a fixed grid.
//!
//! Pages are read-only aggregations, drawn with exactly the table chrome of the table view -
//! a pane row is a table row, so everything the administration spec does to a row works inside
//! a pane unchanged.

use std::time::Duration;

use nutsh_catalog::{Column, Kind, PageDef, PaneDef};
use nutsh_core::scheduler::{Scheduler, SubId, Subscription, pane_budget};
use nutsh_core::store::TableKey;

pub mod summary;

pub use summary::SummaryLine;

pub struct Pane {
    pub def: &'static PaneDef,
    pub key: TableKey,
    /// `None` while the page is buried: a page under another view stops polling.
    pub sub: Option<SubId>,
    pub selected: usize,
    /// The pane's own reason instead of rows - a namespace this Prism Central does not serve,
    /// a kind that is not in the version it pins, or a 404 on a sub-path the spec declares and
    /// the server does not have. Drawn inside the pane's block, not as an error banner:
    /// `multidomain` sub-paths are known to 404 on real Prism Centrals even when the spec
    /// declares them.
    ///
    /// Set at construction and by [`PageView::regrade`], which is what lets an answer that
    /// arrives *after* the page opened grey the pane it was about.
    pub reason: Option<String>,
    /// Rows the last frame that drew this pane had room for. `ui::draw_pane` is its only
    /// writer, and `ui::draw_page` zeroes the panes it has no room to draw, so a pane in a
    /// dropped grid row falls back to the floor instead of polling at the height it had when
    /// it was last visible. Crate-internal for the reason `sampler` is: a writer outside the
    /// crate could force a resubscribe. A `Cell` for the reason `App::width` is one:
    /// `ui::draw` takes `&App`, and the layout is only known while it draws.
    pub(crate) drawn: std::cell::Cell<usize>,
    /// The row budget the pane's current subscription carries, copied off it at [`subscribe`]:
    /// what the pane last *asked for*, not the height it asked at. [`PageView::resize`]
    /// compares against this rather than against `drawn`, because the floor and the `$limit`
    /// cap both collapse a range of heights onto one request, and a pane that grew from ten
    /// rows to twelve must not resubscribe to fetch the same twenty. `None` until the pane
    /// has a subscription at all.
    asked_rows: Option<u32>,
    /// `ctrl-x` stopped this pane's walk, on the same terms as `TableView::stopped`.
    pub stopped: bool,
}

impl Pane {
    /// How many rows this pane's walk will actually fetch: what its subscription asked for,
    /// falling back to the kind's budget until it has one.
    ///
    /// The body's `listing up to N rows…` promises this number, and it is not the kind's:
    /// [`subscribe`] narrows the budget to the pane's own height through `Subscription::sized`,
    /// so the Dashboard's alerts pane asks for twenty of the catalog's curated five hundred.
    pub fn budget(&self) -> Option<u32> {
        self.asked_rows.or(self.key.kind.max_rows)
    }

    /// The columns the pane draws: its own when it declares any, else the kind's default six.
    pub fn columns(&self) -> &'static [Column] {
        if self.def.columns.is_empty() {
            crate::table::columns(self.key.kind, false)
        } else {
            self.def.columns
        }
    }
}

pub struct PageView {
    pub def: &'static PageDef,
    pub panes: Vec<Pane>,
    pub focus: usize,
    pub summary: Vec<SummaryLine>,
    /// The page's own poller, started and stopped with the panes it feeds. Crate-internal like
    /// `focused_mut`, and for a sharper reason: a caller outside could take the handle and
    /// leak the task, which nothing but this type's own `Drop` would then stop.
    pub(crate) sampler: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for PageView {
    fn drop(&mut self) {
        // The backstop under `release`, for the paths that drop a page without releasing it:
        // `App::attach` replaces `Live` wholesale on a context switch, and a bare
        // `tokio::spawn` would outlive the session.
        if let Some(handle) = &self.sampler {
            handle.abort();
        }
    }
}

impl PageView {
    /// Every pane subscribed. `unavailable` supplies the session's reason for a kind this
    /// Prism Central does not serve; such a pane is built without a subscription and says so.
    pub fn open(
        def: &'static PageDef,
        scheduler: &mut Scheduler,
        unavailable: impl Fn(&'static Kind) -> Option<String>,
    ) -> PageView {
        let panes = def
            .panes
            .iter()
            .filter_map(|d| {
                let kind = nutsh_catalog::kind(d.kind)?;
                let key = TableKey::filtered(kind, d.filter);
                let reason = unavailable(kind);
                // Nothing has drawn it yet, so it subscribes at a height of zero: twenty rows
                // and one page, which is what a pane asks for before a frame has said how
                // tall it is. `App::tick` resizes it once one has drawn it taller.
                let mut pane = Pane {
                    def: d,
                    key,
                    sub: None,
                    selected: 0,
                    reason,
                    drawn: std::cell::Cell::new(0),
                    asked_rows: None,
                    stopped: false,
                };
                if pane.reason.is_none() {
                    pane.sub = Some(subscribe(scheduler, &mut pane));
                }
                Some(pane)
            })
            .collect();
        PageView {
            def,
            panes,
            focus: 0,
            summary: Vec::new(),
            sampler: None,
        }
    }

    /// How many panes are polling; 0 while the page is buried.
    pub fn live_panes(&self) -> usize {
        self.panes.iter().filter(|p| p.sub.is_some()).count()
    }

    /// Whether the page's own poller is running; false while the page is buried, and false on
    /// every page that declares no sampler. The counterpart of `live_panes` for the one poller
    /// the scheduler knows nothing about.
    pub fn sampling(&self) -> bool {
        self.sampler.is_some()
    }

    /// Stop polling: a view was pushed over this page.
    ///
    /// The sampler stops with the panes. A page under a table is a screen nobody is looking at,
    /// and the sampler's cycle is the most expensive thing the page does: a list and up to
    /// fifty gets. `App::pop` starts it again where it re-subscribes the panes.
    pub fn release(&mut self, scheduler: &mut Scheduler) {
        for pane in &mut self.panes {
            if let Some(sub) = pane.sub.take() {
                scheduler.unsubscribe(sub);
            }
        }
        if let Some(handle) = self.sampler.take() {
            handle.abort();
        }
    }

    /// Take the session's verdict again for every pane that has none, now that something has
    /// answered. A reason settled at construction would grey a pane only where the Prism
    /// Central had already been asked about that kind before the page opened. A list 404 arriving from a live poll is exactly the answer this is
    /// for - the client remembers it, and this is what turns the memory into a greyed pane and
    /// one fewer subscription.
    ///
    /// One-way, like the store's `not_served`: a pane that has a reason keeps it until the
    /// page is opened afresh, and [`PageView::reacquire`] leaves it alone. The rows it already
    /// drew are the store's and stay there; `draw_pane` prefers them to the sentence.
    pub fn regrade(
        &mut self,
        scheduler: &mut Scheduler,
        unavailable: impl Fn(&'static Kind) -> Option<String>,
    ) {
        for pane in &mut self.panes {
            if pane.reason.is_some() {
                continue;
            }
            let Some(reason) = unavailable(pane.key.kind) else {
                continue;
            };
            pane.reason = Some(reason);
            if let Some(sub) = pane.sub.take() {
                scheduler.unsubscribe(sub);
            }
        }
    }

    /// Poll again: the view above was popped. The rows the page was buried with are held over
    /// until the new subscription's first cycle lands - `Store::loading` is one-way, so a
    /// table that has completed a cycle never says it is loading again - and the new
    /// subscription's generation is what decides which cycle replaces them.
    pub fn reacquire(&mut self, scheduler: &mut Scheduler) {
        for pane in &mut self.panes {
            if pane.sub.is_none() && pane.reason.is_none() {
                let sub = subscribe(scheduler, pane);
                pane.sub = Some(sub);
            }
        }
    }

    /// Re-subscribe any pane the last frame drew taller than its subscription asks for: a pane
    /// redrawn taller has to fetch the rows it can now show. Called from `App::tick`, which is
    /// where a resize has settled by.
    ///
    /// **Grow eagerly, shrink lazily.** A resubscribe is not free: `Scheduler::unsubscribe`
    /// aborts whatever walk was in flight, its rate-limit token already spent, and
    /// `Scheduler::subscribe` runs a cycle immediately. A pane redrawn *shorter* already holds
    /// every row it can draw, so asking for fewer would throw a walk away to save nothing -
    /// and a terminal edge being dragged would resubscribe the whole grid once a second, in
    /// the one place whose whole point is fewer requests. A shrink therefore rides until the
    /// pane next grows past what it asked for, or until [`PageView::reacquire`] rebuilds the
    /// subscription from the height it is at now.
    ///
    /// Resubscribing rather than mutating the subscription in place: a `Subscription` is
    /// cloned into its task at `Scheduler::subscribe`, so the task holding the old budget is
    /// the old task, and the new one starts a fresh cycle - unarmed, so it walks rather than
    /// probing its way past rows the pane could not show before.
    pub(crate) fn resize(&mut self, scheduler: &mut Scheduler) {
        for pane in &mut self.panes {
            let Some(old) = pane.sub else { continue };
            let (_, needed) = pane_budget(pane.drawn.get());
            if pane.asked_rows.is_some_and(|asked| needed <= asked) {
                continue;
            }
            scheduler.unsubscribe(old);
            let sub = subscribe(scheduler, pane);
            pane.sub = Some(sub);
        }
    }

    /// Which pane a subscription belongs to, for the app's message routing.
    pub fn pane_of(&self, sub: SubId) -> Option<usize> {
        self.panes.iter().position(|p| p.sub == Some(sub))
    }

    pub fn focused(&self) -> Option<&Pane> {
        self.panes.get(self.focus)
    }

    /// Crate-internal, like `Live::page_mut`: the app clamps a pane's selection against the
    /// rows it actually has, and a caller outside would move the cursor past them.
    pub(crate) fn focused_mut(&mut self) -> Option<&mut Pane> {
        self.panes.get_mut(self.focus)
    }

    pub fn cycle(&mut self, forward: bool) {
        let n = self.panes.len();
        if n == 0 {
            return;
        }
        self.focus = if forward {
            (self.focus + 1) % n
        } else {
            (self.focus + n - 1) % n
        };
    }
}

/// One pane's subscription: the kind's own poll interval, the pane's query, and a walk the
/// size of the pane - the rows its last frame had room for plus headroom, floored at twenty.
/// The budget is read back off the subscription onto the pane, so [`PageView::resize`] can
/// tell a pane redrawn taller from one merely redrawn, and so the two can never disagree:
/// `pane_budget` is consulted once per subscribe, inside `Subscription::sized`.
///
/// **A pane's rows are a slice, and the store does not say so.** A pane that declares no
/// filter has the same `TableKey` as the table view of its kind - `TableKey::filtered(vm,
/// None)` is `TableKey::top(vm)`, which is exactly the Dashboard's `VMs` pane - so the two
/// share one entry in the store, and a completed pane cycle leaves that entry holding twenty
/// rows with `loading` false. Nothing on `Table` records which budget wrote them (generations
/// order cycles; they do not measure them), so every reader of that entry has to survive
/// finding a slice there:
///
/// - The **table view** never runs beside a pane: a page is always the bottom of the stack,
///   and `push_view`/`drain_stack` release it before anything is pushed over it. The table
///   view's own subscription carries the kind's budget and refills the entry on its first
///   cycle, with the pane's rows shown until it lands.
/// - The **reference picker** cannot tell a short table from a settled one by `loading`, so
///   `App::open_ref_picker` compares the rows against the server's `total` instead and asks
///   again when they are a slice.
/// - The **cache writer** persists whatever the last cycle left, so a run that ended on a page
///   restores the pane's rows rather than the kind's as the next run's first frame. The live
///   cycle refills them, and `total` is the server's throughout, so the title says `20/500`
///   either way.
fn subscribe(scheduler: &mut Scheduler, pane: &mut Pane) -> SubId {
    let sub = Subscription::pane(
        pane.key.clone(),
        Duration::from_secs(u64::from(pane.key.kind.poll_secs.max(1))),
        pane.def.filter.map(str::to_string),
        pane.def.orderby.map(str::to_string),
    )
    .sized(pane.drawn.get());
    pane.asked_rows = sub.max_rows;
    scheduler.subscribe(sub)
}
