//! Tables of entities by kind and parents, updated by generation-tagged messages.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use indexmap::IndexMap;
use nutsh_catalog::Kind;
use nutsh_prism::{Entity, NOT_SERVED, PrismError};

use crate::cell::Names;
use crate::stats::Stats;

/// One table: a kind listed under a chain of parent ids (empty for a top-level kind), with the
/// `$filter` it was listed with when it is a page pane's.
///
/// `Kind` does not derive `Hash` (it derives only `Debug, PartialEq, Eq`), so `Eq` and `Hash`
/// are implemented by hand below, over `(kind.id, &parents, filter)` rather than over every
/// field of `Kind` itself.
#[derive(Debug, Clone)]
pub struct TableKey {
    pub kind: &'static Kind,
    pub parents: Vec<String>,
    /// The OData `$filter` this table was listed with: a page pane's, or a search's. Part of
    /// the identity - `prism.config.Task` filtered to RUNNING and the same kind unfiltered are
    /// two tables, and merging them would show a pane rows its own filter excludes.
    ///
    /// Owned, because a search builds its filter from what was typed. The cache has always
    /// stored this as a `String`; only the key insisted on a literal, and that came from
    /// filters having had one source.
    pub filter: Option<std::sync::Arc<str>>,
    /// The context this table was listed from, when it is not the session's own: a peer joined
    /// with `:ctx a b`. `None` is the primary session, which every constructor builds.
    pub context: Option<std::sync::Arc<str>>,
}

impl TableKey {
    /// A top-level table, listed whole: what a table view opens.
    pub fn top(kind: &'static Kind) -> TableKey {
        TableKey {
            kind,
            parents: Vec::new(),
            filter: None,
            context: None,
        }
    }

    /// A child table under the chain of parent ids that fills its list path.
    pub fn under(kind: &'static Kind, parents: Vec<String>) -> TableKey {
        TableKey {
            kind,
            parents,
            filter: None,
            context: None,
        }
    }

    /// The same table, listed from a peer context.
    pub fn in_context(mut self, name: std::sync::Arc<str>) -> TableKey {
        self.context = Some(name);
        self
    }

    /// A top-level table of the primary session, listed whole: the one a peer's rows merge into.
    pub fn is_top(&self) -> bool {
        self.parents.is_empty() && self.filter.is_none() && self.context.is_none()
    }

    /// A page pane's table: top-level, and keyed by its own filter as well as its kind.
    pub fn filtered(kind: &'static Kind, filter: Option<std::sync::Arc<str>>) -> TableKey {
        TableKey {
            kind,
            parents: Vec::new(),
            filter,
            context: None,
        }
    }
}

impl PartialEq for TableKey {
    fn eq(&self, other: &Self) -> bool {
        self.kind.id == other.kind.id
            && self.parents == other.parents
            && self.filter == other.filter
            && self.context == other.context
    }
}

impl Eq for TableKey {}

impl Hash for TableKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind.id.hash(state);
        self.parents.hash(state);
        self.filter.hash(state);
        self.context.hash(state);
    }
}

/// Why a cycle failed, in the terms the screen has to tell apart, with the server's own words
/// kept beside them rather than instead of them.
///
/// A `String` crossing this seam - `Msg::Error { error: e.to_string() }` - leaves nothing to
/// reword by the time it reaches a table, a pane, a detail pane or the status line: those four
/// surfaces would draw whatever the far end had written, including an API gateway's "The
/// requested URL was not found on the server. If you entered the URL manually please check your
/// spelling and try again.", which in a TUI names a URL nobody typed and a spelling nobody can
/// check. The variant survives to the screen instead, and [`Failure::text`] is the only thing
/// the screen draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub cause: Cause,
    /// The namespace the request was for, so a service outage can name the service.
    pub namespace: &'static str,
    /// The error as it stands, whole: this crate's own words with the far end's inside them.
    /// The only place the far end's survive - `:journal` and a debug log, where they mean
    /// something to whoever is reading a trace - and never the table, the pane, the detail
    /// pane or the status line.
    pub upstream: String,
}

/// The distinction the whole type exists for: **not here** against **not now**. A 404 is
/// terminal - the scheduler stops the subscription and the entry greys - and a 5xx is the
/// service having a bad minute, which is retried and said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// 404 on a kind's own list path: this Prism Central does not have the endpoint.
    NotServed,
    /// 404 on a row or on a child list: what was asked for has gone, not the endpoint.
    NotFound,
    /// 5xx. The service, not the request.
    Unavailable(u16),
    /// 429, or the local budget standing in for one.
    RateLimited,
    /// Any other status the far end named: a 400 for a parameter this client got wrong, a 403,
    /// a 405. The status is the whole of what can be said without quoting the server.
    Rejected(u16),
    /// A 401 from this endpoint alone, on a session that still answers elsewhere: the account
    /// may not read it. Ends the subscription the way a 403 would; the credential is fine.
    Denied,
    /// The request got no answer at all: DNS, a connection lost mid-session, a timeout, a
    /// reset.
    Unreachable,
    /// An answer arrived that this client could not read.
    Unreadable,
    /// A failure this program decided on rather than one a server answered with: a body that
    /// is missing, a plan that could not be built. The one cause whose sentence *is*
    /// `upstream`, because that sentence is this program's own.
    Local,
}

impl Failure {
    /// `not_served` is the scheduler's verdict on which 404 is the endpoint's: only it knows
    /// whether the path carried a parent's ext id. See `is_missing_list` there.
    pub fn of(kind: &'static Kind, e: &PrismError, not_served: bool) -> Failure {
        let cause = match e {
            PrismError::NotFound(_) if not_served => Cause::NotServed,
            PrismError::NotFound(_) => Cause::NotFound,
            PrismError::Api {
                status: status @ 500..,
                ..
            } => Cause::Unavailable(*status),
            PrismError::RateLimited { .. } => Cause::RateLimited,
            PrismError::Auth => Cause::Rejected(401),
            PrismError::Denied(_) => Cause::Denied,
            PrismError::Forbidden(_) => Cause::Rejected(403),
            PrismError::Conflict { .. } => Cause::Rejected(412),
            PrismError::Api { status, .. } => Cause::Rejected(*status),
            // `reqwest`'s prose is no better in a table than an API gateway's: "error sending
            // request for url (https://…/api/…): operation timed out" names a URL nobody
            // typed, exactly as the 404 that started this rule did.
            PrismError::Transport(_) => Cause::Unreachable,
            PrismError::Decode(_) => Cause::Unreadable,
            _ => Cause::Local,
        };
        Failure {
            cause,
            namespace: kind.namespace,
            upstream: e.to_string(),
        }
    }

    /// A failure this program decided on rather than one a server answered with: a body that
    /// is missing, a plan that could not be built. Its words are already the house's.
    pub fn local(message: impl Into<String>) -> Failure {
        Failure {
            cause: Cause::Local,
            namespace: "",
            upstream: message.into(),
        }
    }

    /// The one sentence the screen draws. Short, factual, specific - and never the server's.
    pub fn text(&self) -> String {
        match self.cause {
            Cause::NotServed => NOT_SERVED.to_string(),
            Cause::NotFound => "not found (HTTP 404)".to_string(),
            // "retrying" is a promise the scheduler keeps: a 5xx backs off and comes round
            // again, where a 404 stops. Saying which is happening is the whole of Part 3.
            Cause::Unavailable(status) => {
                format!("{} unavailable (HTTP {status}) - retrying", self.namespace)
            }
            Cause::RateLimited => "rate limited (HTTP 429) - retrying".to_string(),
            // The one sentence in this table that names a remedy, because it is the one
            // failure the reader can do something about and the only one where the program has
            // *stopped*. Everything else here is still being retried.
            Cause::Rejected(401) => {
                "authentication failed (HTTP 401) - requests stopped; run `nutsh ctx login`"
                    .to_string()
            }
            Cause::Rejected(403) => "not permitted for this account (HTTP 403)".to_string(),
            Cause::Denied => "not permitted for this account (HTTP 401)".to_string(),
            // A redirect is now an answer rather than a hop - `Client::connect` follows none,
            // because following one carries the session cookie to whatever host the `Location`
            // named - and "refused" would be the wrong word for it. Prism's `/api/*` has no
            // legitimate redirect, so this is a Prism Central pointed somewhere else.
            Cause::Rejected(status @ 300..=399) => {
                format!("this Prism Central redirected the request (HTTP {status}) - not followed")
            }
            Cause::Rejected(status) => format!("the request was refused (HTTP {status})"),
            Cause::Unreachable => "cannot reach this Prism Central (retrying)".to_string(),
            // Named rather than quoted, and it names where the quote went: a malformed body is
            // a developer's problem, and `:journal` is where a developer looks.
            Cause::Unreadable => "the response could not be read (see :journal)".to_string(),
            Cause::Local => self.upstream.clone(),
        }
    }

    /// Whether asking again could only produce the same answer **and** cost something real to
    /// ask: a credential this Prism Central has refused.
    ///
    /// The sibling of [`Failure::is_not_served`], and a sharper rule than it. A 404 retried for
    /// ever is wasted requests behind a screen nobody is watching; a password retried for ever
    /// is failures counted against a real account, and this program has locked out a real
    /// administrator twice. One spelling of the test, so the scheduler and the store cannot
    /// drift about which failure ends a subscription.
    pub fn is_terminal(&self) -> bool {
        matches!(self.cause, Cause::Rejected(401) | Cause::Denied)
    }

    /// Whether this is the endpoint's absence: what sets `Table::not_served` and stops a
    /// subscription. One spelling of the test, so the store and the scheduler cannot drift.
    pub fn is_not_served(&self) -> bool {
        self.cause == Cause::NotServed
    }
}

/// What a polling cycle reports. `generation` numbers cycles per table; anything older than
/// the table's current cycle is ignored.
#[derive(Debug)]
pub enum Update {
    /// A listing cycle has begun. It carries no data: its whole job is to let the frame say
    /// `listing…` and to anchor the elapsed time, before the first page can possibly arrive.
    Started { generation: u64 },
    /// One page of a listing cycle; pages accumulate as staging until `Complete` replaces the
    /// rows.
    Page {
        generation: u64,
        entities: Vec<Entity>,
        total: Option<u64>,
    },
    /// The cycle finished: rows become whatever staged for this generation, if anything did.
    Complete { generation: u64 },
    /// One entity refreshed out of band (e.g. a live subscription); never advances the generation.
    Entity { generation: u64, entity: Entity },
    /// The cycle failed; the rows stay, and any staging in progress for it is discarded.
    Error { generation: u64, error: Failure },
}

/// Pages staged for one in-progress generation, not yet visible as rows.
#[derive(Clone, Debug)]
struct Staging {
    generation: u64,
    entities: Vec<Entity>,
    total: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    /// By extId, in server order.
    pub rows: IndexMap<String, Entity>,
    /// The last completed list cycle. Single-entity cycles refresh `last_poll` but leave it.
    pub generation: u64,
    pub total: Option<u64>,
    pub last_poll: Option<Instant>,
    /// The last cycle's failure; the rows stay. Typed, so the screen chooses the words: see
    /// [`Failure`].
    pub error: Option<Failure>,
    /// A 404 on the list path: this Prism Central does not have the endpoint. Set by the
    /// scheduler, which then stops polling, and cleared by a cycle that **completes** -
    /// never by another failure, however it answered.
    ///
    /// One-way until then because a detail view subscribes `Subscription::single` under the
    /// same key as the table it sits over: a gone entity, or one transient error on the
    /// detail's own cycle, would otherwise write `not_served: false` over a standing flag
    /// whose list subscription has already stopped, and nothing would ever set it back.
    pub not_served: bool,
    /// No cycle has completed yet. One-way: a later refresh in flight does not set it again.
    pub loading: bool,
    /// The in-flight listing cycle's generation and when it began. Set by [`Update::Started`],
    /// cleared by the `Complete` or `Error` **of that same generation**. A late ending from an
    /// older or an unrelated cycle leaves it alone; [`Store::abandon`] is what ends a cycle whose
    /// own ending will never arrive because its subscription was dropped.
    ///
    /// The generation rides in the field because `Table::generation` is the last *completed*
    /// cycle and nothing else records the in-flight one: a bare `Option<Instant>` cleared on any
    /// `Complete` would let a late completion from an overtaken cycle blank the newer cycle's
    /// clock, and the title would lose its elapsed time mid-walk. So would a single-entity
    /// refresh beside the walk - every subscription draws generations from one counter, so a
    /// `once` issued after a walk began always carries a higher number than it.
    pub cycle_started: Option<(u64, Instant)>,
    /// When the rows on screen were written to the cache, in unix seconds, while they are
    /// still the ones the cache held. `None` once a cycle has replaced them - not merely once
    /// one has polled: a single-entity `Complete` stages nothing, so it leaves both the
    /// restored rows and the badge that explains them. Read by the TUI's `sync_indicator`,
    /// which turns it into `◌ cached 12m`.
    pub restored_at: Option<u64>,
    /// The one document a single-entity GET fetched, kept beside the rows so a narrowed list
    /// cannot half-empty a detail drawn over it.
    ///
    /// A list carrying `$select` asks for the columns and gets back a row with nothing else in
    /// it, and [`Update::Complete`] replaces `rows` wholesale. A composed detail is drawn over
    /// the row the store holds, so without somewhere else to keep the whole document the pane
    /// would go from complete to half empty and back on every poll. [`Table::row`] is what
    /// reads it, and it prefers it over the list row precisely because it is the fuller one.
    ///
    /// One entry, not a map: the pane is open over exactly one entity at a time, and a
    /// subscription bounded by what is on screen cannot grow. A post-action `once` refresh
    /// replaces it with the row it acted on, which is the row the cursor is on.
    whole: Option<Entity>,
    /// Pages accumulating for a generation that has not completed yet.
    staging: Option<Staging>,
}

/// Every table this session knows about, plus the shared extId → name cache they feed.
#[derive(Debug)]
pub struct Store {
    tables: HashMap<TableKey, Table>,
    names: Names,
    /// A placeholder in the loading state, returned by `table` for a key with no entry yet.
    empty: Table,
    /// Session-wide counters, replaced wholesale by the stats poller.
    stats: Stats,
    /// The Disaster Recovery page's counters and tallies, replaced wholesale by its sampler.
    sample: crate::sampler::ProtectionSample,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            tables: HashMap::new(),
            names: Names::default(),
            empty: Table::fresh(),
            stats: Stats::default(),
            sample: crate::sampler::ProtectionSample::default(),
        }
    }
}

impl Store {
    /// The table, or an empty placeholder in the loading state, so views never branch on absence.
    pub fn table(&self, key: &TableKey) -> &Table {
        self.tables.get(key).unwrap_or(&self.empty)
    }

    pub fn names(&self) -> &Names {
        &self.names
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn set_stats(&mut self, stats: Stats) {
        self.stats = stats;
    }

    /// Names from the warm-up. Last write wins, exactly as a table poll's do: both sources
    /// read the entity live, so neither is staler than the other, and a rename propagates from
    /// whichever runs next. Nothing is ever removed - a name that came from a table the user
    /// has since left is still the right name.
    ///
    /// The cache's map comes back through here too, before the first frame: a later poll
    /// overwrites a restored entry, and a restore never overwrites a poll, because it runs
    /// before any poll can have landed.
    pub fn apply_names(&mut self, names: Vec<(String, String)>) {
        for (ext_id, name) in names {
            self.names.insert(ext_id, name);
        }
    }

    pub fn sample(&self) -> &crate::sampler::ProtectionSample {
        &self.sample
    }

    pub fn set_sample(&mut self, sample: crate::sampler::ProtectionSample) {
        self.sample = sample;
    }

    pub fn apply(&mut self, key: &TableKey, update: Update) {
        let table = self.tables.entry(key.clone()).or_insert_with(Table::fresh);
        match update {
            Update::Started { generation } => {
                // Newer than the last completed cycle, and newer than any cycle already in
                // flight: a `Started` that lost a race must not move the clock back.
                if generation <= table.generation {
                    return;
                }
                if table.cycle_started.is_some_and(|(g, _)| g >= generation) {
                    return;
                }
                table.cycle_started = Some((generation, Instant::now()));
            }
            Update::Page {
                generation,
                entities,
                total,
            } => {
                // A completed generation never re-stages; a newer cycle already staging wins
                // over a late page from an older one.
                if generation <= table.generation {
                    return;
                }
                if table
                    .staging
                    .as_ref()
                    .is_some_and(|s| s.generation > generation)
                {
                    return;
                }
                if table
                    .staging
                    .as_ref()
                    .is_none_or(|s| s.generation < generation)
                {
                    table.staging = Some(Staging {
                        generation,
                        entities: Vec::new(),
                        total: None,
                    });
                }
                let staging = table
                    .staging
                    .as_mut()
                    .expect("just set above if absent or stale");
                staging.entities.extend(entities);
                staging.total = total;
            }
            Update::Complete { generation } => {
                if generation < table.generation {
                    return;
                }
                // The clock belongs to the cycle that set it, and only that cycle's own ending
                // clears it: an overtaken cycle completing late must not blank the walk that
                // replaced it, and neither must a single-entity `Complete` beside the walk -
                // generations come from one shared counter, so a `once` issued after the walk
                // began always carries a higher number than it.
                if table.cycle_started.is_some_and(|(g, _)| g == generation) {
                    table.cycle_started = None;
                }
                // A `Complete` for a generation whose pages are not the ones staged leaves the
                // rows alone: the single-entity case (nothing ever staged) and the
                // late-completion case (a newer cycle has already taken over staging).
                if let Some(s) = table.staging.take_if(|s| s.generation == generation) {
                    table.rows = index(s.entities);
                    for e in table.rows.values() {
                        self.names.insert(e.ext_id.clone(), e.name.clone());
                    }
                    table.total = s.total;
                    // The rows on screen are this cycle's now, not the cache's.
                    table.restored_at = None;
                    // Only a cycle that replaced the rows moves the generation: a single-entity
                    // subscription on the same table completes with nothing staged, and if it
                    // advanced the generation the list cycle in flight beside it would be
                    // dropped as stale.
                    table.generation = generation;
                }
                table.last_poll = Some(Instant::now());
                table.error = None;
                table.not_served = false;
                table.loading = false;
            }
            Update::Entity { generation, entity } => {
                if generation < table.generation {
                    return;
                }
                let ext_id = entity.ext_id.clone();
                self.names.insert(ext_id.clone(), entity.name.clone());
                // Only [`fetch_one`] raises an `Entity`, and it raises what `Client::get_in`
                // answered: a whole document, whatever the list beside it was narrowed to.
                table.whole = Some(entity.clone());
                let grew = table.rows.insert(ext_id, entity).is_none();
                if grew {
                    table.total = table.total.map(|t| t + 1);
                }
                table.loading = false;
            }
            Update::Error { generation, error } => {
                if generation < table.generation {
                    return;
                }
                // The clock belongs to the cycle that set it, on the same terms as `Complete`
                // above: only its own failure clears it.
                if table.cycle_started.is_some_and(|(g, _)| g == generation) {
                    table.cycle_started = None;
                }
                // Set, never cleared: see the field. Only a `Complete` above clears it.
                table.not_served |= error.is_not_served();
                table.error = Some(error);
                // A failed cycle produced nothing usable.
                // Only a failure of the staging cycle itself, or of an older one, discards the
                // staged pages; a late error from a superseded cycle must not touch a newer one.
                if table
                    .staging
                    .as_ref()
                    .is_some_and(|s| s.generation <= generation)
                {
                    table.staging = None;
                }
                table.loading = false;
            }
        }
    }

    /// Paint rows the cache held. Not an `Update`: a restore is a new entry point, and a
    /// fresh table's generation of 0 is what keeps it out of `apply`'s staging rules
    /// altogether - every live cycle's generation is at least 1, so the first live `Complete`
    /// always wins.
    ///
    /// A head start, never a rewind: a table that has already run a live cycle is left exactly
    /// as it is, because lowering its generation back to 0 would re-admit a superseded cycle's
    /// pages and badge live rows as the cache's.
    ///
    /// `rows` are `Entity`s the caller built with `Entity::new`, so `ext_id` and `name` are
    /// re-derived through the **current** catalog and a `name_path` curated since the write is
    /// honoured. A row with no ext id has no identity and is dropped.
    ///
    /// Returns the number of rows kept after the ext-id filter, for the caller's log line;
    /// 0 means nothing was painted and the table was not touched.
    #[must_use]
    pub fn restore(
        &mut self,
        key: &TableKey,
        rows: Vec<Entity>,
        total: Option<u64>,
        at: u64,
    ) -> usize {
        let rows = index(rows);
        // Nothing survived the ext-id filter: leave the table `○ syncing` rather than badging
        // an empty one as the cache's.
        if rows.is_empty() {
            return 0;
        }
        let table = self.tables.entry(key.clone()).or_insert_with(Table::fresh);
        // A head start, never a rewind: a table that has already completed a live cycle keeps
        // its rows, its generation and its staging. A fresh table's generation is already 0.
        if table.generation > 0 || table.last_poll.is_some() {
            return 0;
        }
        table.rows = rows;
        for e in table.rows.values() {
            self.names.insert(e.ext_id.clone(), e.name.clone());
        }
        table.total = total;
        table.restored_at = Some(at);
        table.error = None;
        // There are rows on screen: `○ syncing` is for a table with nothing in it.
        table.loading = false;
        table.rows.len()
    }

    /// End a cycle nobody is waiting for. The view that subscribed is gone and its task is
    /// aborted, so the `Complete` or `Error` that would have cleared the clock will never
    /// arrive: without this a popped table keeps a clock that ticks forever, and the next visit
    /// draws `listing…` and the abandoned walk's `staged_total` over rows that are not moving.
    ///
    /// The rows stay - a popped table is redrawn from them on the next visit, on purpose - and
    /// so does `total`: what is dropped is only what described the walk that was cut short.
    pub fn abandon(&mut self, key: &TableKey) {
        if let Some(table) = self.tables.get_mut(key) {
            table.cycle_started = None;
            table.staging = None;
        }
    }

    /// Tests only: the in-flight cycle's clock on one table, so a progress test can put a walk
    /// three seconds in without a server that takes three seconds to answer. There is no way to
    /// unset it here - `abandon` is that, and it is not a test hook.
    #[cfg(feature = "testing")]
    pub fn set_cycle_started(&mut self, key: &TableKey, started: (u64, Instant)) {
        self.tables
            .entry(key.clone())
            .or_insert_with(Table::fresh)
            .cycle_started = Some(started);
    }

    /// Every table this session knows about, for the cache writer. Read-only: nothing outside
    /// changes a `Table` except through `apply` and `restore`.
    pub fn tables(&self) -> impl Iterator<Item = (&TableKey, &Table)> {
        self.tables.iter()
    }
}

/// Rows keyed by extId, in the order given. A row with no ext id has no identity, no key and
/// no way to be drilled into, so it is dropped rather than collapsing every such row onto one
/// shared `""` entry.
fn index(entities: impl IntoIterator<Item = Entity>) -> IndexMap<String, Entity> {
    entities
        .into_iter()
        .filter(|e| !e.ext_id.is_empty())
        .map(|e| (e.ext_id.clone(), e))
        .collect()
}

impl Table {
    fn fresh() -> Table {
        Table {
            loading: true,
            ..Table::default()
        }
    }

    /// The fullest document this table holds for `ext_id`: what a single-entity GET fetched if
    /// one has, and the list row otherwise.
    ///
    /// What a composed detail reads. `rows` is what the list said, and a list may have been
    /// narrowed by `$select` down to the columns the table draws; this is the one place that
    /// knows the difference.
    pub fn row(&self, ext_id: &str) -> Option<&Entity> {
        self.whole
            .as_ref()
            .filter(|e| e.ext_id == ext_id)
            .or_else(|| self.rows.get(ext_id))
    }

    /// Rows staged for the cycle in flight, not yet visible as rows.
    pub fn staged(&self) -> usize {
        self.staging.as_ref().map_or(0, |s| s.entities.len())
    }

    /// The server's total as the cycle in flight last reported it.
    ///
    /// The table title's first bracket reads `staged_total().or(total)`, and without this
    /// `[0/181825]` is unreachable: `Table::total` stays `None` until a `Complete` stages, so a
    /// table walking its first cycle would show a bare `0` for the whole walk and the server's
    /// own count - the most useful number on the screen - would arrive only once it stopped
    /// mattering. This one first and `total` second, in that order: `total` is the *last
    /// completed* cycle's number, so a table a cycle emptied would otherwise stay pinned at
    /// that `0` for the whole of the next walk.
    pub fn staged_total(&self) -> Option<u64> {
        self.staging.as_ref().and_then(|s| s.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nutsh_catalog::kind;
    use serde_json::json;

    fn vm(id: &str, name: &str) -> Entity {
        Entity::new(
            kind("vmm.ahv.config.Vm").unwrap(),
            json!({"extId": id, "name": name}),
            None,
        )
    }

    fn key() -> TableKey {
        TableKey::top(kind("vmm.ahv.config.Vm").unwrap())
    }

    /// The document a single-entity GET fetched outlives the list cycle beside it.
    ///
    /// This is what makes `$select` safe. A narrowed list asks for the columns and gets a row
    /// with nothing else in it; the detail open over that row is composed from the whole
    /// document a `get_in` fetched, and a `Complete` replaces the row map wholesale. Without
    /// somewhere else to keep it, the pane would go from complete to half empty and back on
    /// every poll.
    #[test]
    fn a_list_cycle_does_not_take_the_whole_row_back_off_the_detail() {
        let mut store = Store::default();
        let k = key();
        let whole = Entity::new(
            kind("vmm.ahv.config.Vm").unwrap(),
            json!({"extId": "a", "name": "A", "disks": [{"backingInfo": {"diskSizeBytes": 42}}]}),
            None,
        );
        store.apply(
            &k,
            Update::Entity {
                generation: 1,
                entity: whole,
            },
        );
        assert!(store.table(&k).row("a").unwrap().raw.get("disks").is_some());

        // A narrowed cycle: the columns and nothing else.
        store.apply(
            &k,
            Update::Page {
                generation: 2,
                entities: vec![vm("a", "A"), vm("b", "B")],
                total: Some(2),
            },
        );
        store.apply(&k, Update::Complete { generation: 2 });

        let t = store.table(&k);
        assert!(
            t.rows["a"].raw.get("disks").is_none(),
            "the table's own row is what the list said"
        );
        assert!(
            t.row("a").unwrap().raw.get("disks").is_some(),
            "the detail's row is still the whole document"
        );
        assert_eq!(
            t.row("b").unwrap().name,
            "B",
            "a row no GET has fetched is the list row"
        );
    }

    #[test]
    fn pages_accumulate_until_complete_then_replace_rows() {
        let mut store = Store::default();
        let k = key();
        assert!(store.table(&k).rows.is_empty());
        assert!(store.table(&k).loading);

        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: Some(3),
            },
        );
        assert!(store.table(&k).rows.is_empty(), "staged, not visible yet");
        assert!(store.table(&k).loading);
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("b", "B"), vm("c", "C")],
                total: Some(3),
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!((t.generation, t.total, t.loading), (1, Some(3), false));
        assert!(t.last_poll.is_some());
        assert_eq!(store.names().get("b"), Some("B"));

        // The next cycle replaces the rows wholesale: `b` is gone.
        store.apply(
            &k,
            Update::Page {
                generation: 2,
                entities: vec![vm("a", "A2"), vm("c", "C")],
                total: Some(2),
            },
        );
        store.apply(&k, Update::Complete { generation: 2 });
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(t.rows["a"].name, "A2");
        assert_eq!(store.names().get("a"), Some("A2"));
    }

    #[test]
    fn older_generations_are_dropped_and_errors_keep_rows() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        store.apply(
            &k,
            Update::Page {
                generation: 0,
                entities: vec![vm("z", "Z")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 0 });
        assert_eq!(store.table(&k).rows.keys().collect::<Vec<_>>(), ["a"]);

        store.apply(
            &k,
            Update::Error {
                generation: 2,
                error: failed("boom"),
            },
        );
        let t = store.table(&k);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.error.as_ref().map(Failure::text).as_deref(), Some("boom"));
        assert!(!t.loading);
        store.apply(
            &k,
            Update::Page {
                generation: 3,
                entities: vec![vm("a", "A")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 3 });
        assert_eq!(store.table(&k).error, None, "a good cycle clears the error");
    }

    #[test]
    fn entity_updates_replace_one_row_without_a_new_generation() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A"), vm("b", "B")],
                total: Some(2),
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        store.apply(
            &k,
            Update::Entity {
                generation: 1,
                entity: vm("a", "A-renamed"),
            },
        );
        let t = store.table(&k);
        assert_eq!(t.rows["a"].name, "A-renamed");
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a", "b"], "order kept");
        assert_eq!(t.generation, 1);
        assert_eq!(
            t.total,
            Some(2),
            "renaming an existing row does not grow the total"
        );
        store.apply(
            &k,
            Update::Entity {
                generation: 1,
                entity: vm("n", "New"),
            },
        );
        let t = store.table(&k);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(
            t.total,
            Some(3),
            "a new row bumps the total so headers stay honest"
        );
        // A single-entity subscription sends `Entity` then `Complete` with nothing staged:
        // the rows must survive that `Complete`.
        store.apply(&k, Update::Complete { generation: 1 });
        let t = store.table(&k);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.total, Some(3));
    }

    #[test]
    fn tables_are_keyed_by_kind_parents_and_filter() {
        let mut store = Store::default();
        let disks = kind("vmm.ahv.config.Disk").unwrap();
        let k1 = TableKey::under(disks, vec!["vm1".into()]);
        let k2 = TableKey::under(disks, vec!["vm2".into()]);
        store.apply(
            &k1,
            Update::Page {
                generation: 1,
                entities: vec![vm("d1", "D1")],
                total: None,
            },
        );
        store.apply(&k1, Update::Complete { generation: 1 });
        assert_eq!(store.table(&k1).rows.len(), 1);
        assert!(store.table(&k2).rows.is_empty());
        // A pane's filtered listing is its own table: merging it with the unfiltered one
        // would show the pane rows its filter excludes.
        let vms = kind("vmm.ahv.config.Vm").unwrap();
        let filtered = TableKey::filtered(vms, Some("powerState eq 'ON'".into()));
        store.apply(
            &filtered,
            Update::Page {
                generation: 2,
                entities: vec![vm("v1", "V1")],
                total: None,
            },
        );
        store.apply(&filtered, Update::Complete { generation: 2 });
        assert_eq!(store.table(&filtered).rows.len(), 1);
        assert!(store.table(&TableKey::top(vms)).rows.is_empty());
    }

    #[test]
    fn single_entity_completes_do_not_advance_the_generation() {
        // A detail view polls one entity on the same table key as the list beside it, with
        // generations from one shared counter: its completions must not outrun the list.
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: Some(1),
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        store.apply(
            &k,
            Update::Entity {
                generation: 3,
                entity: vm("a", "A3"),
            },
        );
        store.apply(&k, Update::Complete { generation: 3 });
        let t = store.table(&k);
        assert_eq!(
            t.generation, 1,
            "nothing was staged, so the list generation stands"
        );
        assert_eq!(t.rows["a"].name, "A3");
        assert!(t.last_poll.is_some());
        // The list cycle that started before the detail completed still lands.
        store.apply(
            &k,
            Update::Page {
                generation: 2,
                entities: vec![vm("b", "B")],
                total: Some(1),
            },
        );
        store.apply(&k, Update::Complete { generation: 2 });
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["b"]);
        assert_eq!(t.generation, 2);
    }

    /// The four sentences the screen is allowed to draw about a failed cycle, and the one
    /// place the server's own prose is kept. A Prism Central's API gateway answers a path it
    /// does not route with "The requested URL was not found on the server. If you entered the
    /// URL manually please check your spelling and try again." - and in a TUI there is no URL
    /// the user entered and no spelling for them to check.
    #[test]
    fn a_failure_says_what_it_is_and_keeps_the_servers_prose_out_of_it() {
        let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
        const GATEWAY: &str = "The requested URL was not found on the server. If you entered \
                               the URL manually please check your spelling and try again.";
        let missing = Failure::of(vm, &PrismError::NotFound(GATEWAY.into()), true);
        assert_eq!(missing.text(), NOT_SERVED);
        assert_eq!(missing.cause, Cause::NotServed);
        assert!(
            missing.upstream.contains(GATEWAY),
            "kept whole for `:journal`, where it means something to a developer: {}",
            missing.upstream
        );
        // The same 404, about one row rather than about the endpoint.
        let gone = Failure::of(vm, &PrismError::NotFound(GATEWAY.into()), false);
        assert_eq!(gone.text(), "not found (HTTP 404)");
        // Not here, versus not now.
        let down = Failure::of(
            vm,
            &PrismError::Api {
                status: 503,
                message: "Failed to perform the operation due to backend service \
                          unavailability, retry after some time."
                    .into(),
                retry_after: None,
            },
            false,
        );
        assert_eq!(down.text(), "vmm unavailable (HTTP 503) - retrying");
        // And a failure of the request itself, whose own words are this program's.
        let refused = Failure::of(
            vm,
            &PrismError::Connect {
                host: "pc.lab".into(),
                message: "connection refused".into(),
            },
            false,
        );
        assert_eq!(
            refused.text(),
            "cannot connect to pc.lab: connection refused"
        );
        // A body this client could not read, and a request that never got an answer. Neither
        // is a sentence a reader can act on, and `reqwest`'s own words are no better in a
        // table than an API gateway's, so both keep their detail for `:journal` too.
        let garbled = Failure::of(
            vm,
            &PrismError::Decode("expected a list, got object".into()),
            false,
        );
        assert_eq!(garbled.cause, Cause::Unreadable);
        assert_eq!(
            garbled.text(),
            "the response could not be read (see :journal)"
        );
        assert!(
            garbled.upstream.contains("expected a list, got object"),
            "{}",
            garbled.upstream
        );
        // A redirect: not a refusal, and the one status the client answers with rather than
        // following, because a followed hop carries the session cookie off this host.
        let moved = Failure::of(
            vm,
            &PrismError::Api {
                status: 302,
                message: "Found".into(),
                retry_after: None,
            },
            false,
        );
        assert_eq!(
            moved.text(),
            "this Prism Central redirected the request (HTTP 302) - not followed"
        );
        let lost = Failure::of(
            vm,
            &PrismError::Transport("error sending request for url".into()),
            false,
        );
        assert_eq!(lost.cause, Cause::Unreachable);
        assert_eq!(lost.text(), "cannot reach this Prism Central (retrying)");
        assert!(
            lost.upstream.contains("error sending request for url"),
            "{}",
            lost.upstream
        );
    }

    /// A failure of this program's own, for a test that only needs *a* failure.
    fn failed(message: &str) -> Failure {
        Failure::local(message)
    }

    /// And the one the store treats specially: the endpoint's own 404.
    fn missing() -> Failure {
        Failure::of(
            nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs"),
            &PrismError::NotFound("no such path".into()),
            true,
        )
    }

    /// The whole of `not_served`'s state machine: a cycle that completes is the only thing
    /// that clears it, and an error that does not carry the flag cannot - a detail view
    /// subscribes under the same key as its table, and its own failure must not speak for the
    /// list.
    #[test]
    fn only_a_completed_cycle_clears_not_served() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Error {
                generation: 1,
                error: missing(),
            },
        );
        assert!(store.table(&k).not_served);

        // The detail beside it, on the same key: one gone entity says nothing about the list.
        store.apply(
            &k,
            Update::Error {
                generation: 2,
                error: failed("gone"),
            },
        );
        assert!(
            store.table(&k).not_served,
            "another failure does not clear the flag"
        );

        // A cycle that answers does.
        store.apply(
            &k,
            Update::Page {
                generation: 3,
                entities: vec![vm("a", "A")],
                total: Some(1),
            },
        );
        store.apply(&k, Update::Complete { generation: 3 });
        let t = store.table(&k);
        assert!(!t.not_served, "a completed cycle clears it");
        assert_eq!(t.error, None);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn a_late_error_from_an_older_cycle_cannot_wipe_newer_staging() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        store.apply(
            &k,
            Update::Page {
                generation: 3,
                entities: vec![vm("x", "X"), vm("y", "Y")],
                total: None,
            },
        );
        store.apply(
            &k,
            Update::Error {
                generation: 2,
                error: failed("late"),
            },
        );
        store.apply(&k, Update::Complete { generation: 3 });
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["x", "y"]);
        assert_eq!(t.generation, 3);
        assert_eq!(t.error, None, "the completed cycle clears the late error");
        // An error of the staging cycle itself does discard its pages.
        store.apply(
            &k,
            Update::Page {
                generation: 4,
                entities: vec![vm("z", "Z")],
                total: None,
            },
        );
        store.apply(
            &k,
            Update::Error {
                generation: 4,
                error: failed("boom"),
            },
        );
        store.apply(&k, Update::Complete { generation: 4 });
        assert_eq!(
            store.table(&k).rows.keys().collect::<Vec<_>>(),
            ["x", "y"],
            "nothing staged survived the error"
        );
    }

    #[test]
    fn a_late_page_from_an_older_cycle_cannot_wipe_newer_staging() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });

        store.apply(
            &k,
            Update::Page {
                generation: 3,
                entities: vec![vm("x", "X"), vm("y", "Y")],
                total: None,
            },
        );
        // A page from an older, already-superseded cycle must not clobber generation 3's staging.
        store.apply(
            &k,
            Update::Page {
                generation: 2,
                entities: vec![vm("z", "Z")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 3 });

        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["x", "y"]);
        assert_eq!(t.generation, 3);
    }

    #[test]
    fn a_repeated_page_for_a_completed_generation_is_ignored() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A"), vm("b", "B")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });

        // A stray, already-completed generation's page must not restart staging or touch rows.
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });

        assert_eq!(store.table(&k).rows.keys().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn stale_entity_and_error_updates_are_dropped() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 2,
                entities: vec![vm("a", "A")],
                total: None,
            },
        );
        store.apply(&k, Update::Complete { generation: 2 });

        store.apply(
            &k,
            Update::Entity {
                generation: 1,
                entity: vm("z", "Z"),
            },
        );
        store.apply(
            &k,
            Update::Error {
                generation: 1,
                error: failed("stale"),
            },
        );

        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(t.error, None);
    }

    #[test]
    fn an_entity_does_not_clear_a_standing_error() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Error {
                generation: 2,
                error: failed("boom"),
            },
        );
        store.apply(
            &k,
            Update::Entity {
                generation: 2,
                entity: vm("a", "A"),
            },
        );
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(
            t.error.as_ref().map(Failure::text).as_deref(),
            Some("boom"),
            "Entity does not clear a standing error"
        );

        // A `Complete` for a key that was never staged yields an empty, non-loading table.
        let other = TableKey::top(kind("vmm.ahv.config.Disk").unwrap());
        store.apply(&other, Update::Complete { generation: 1 });
        let t = store.table(&other);
        assert!(t.rows.is_empty());
        assert!(!t.loading);
    }

    /// A restore is a new entry point, not a new `Update`. Generation 0 is what makes it need
    /// no special case anywhere in `apply`: every live cycle's generation is at least 1,
    /// because `generations` is a session-wide `AtomicU64` starting at 0 and `run` uses
    /// `fetch_add(1) + 1`, so the first live `Complete` always wins over the restore.
    #[test]
    fn a_restore_paints_rows_that_the_first_live_cycle_then_replaces() {
        let mut store = Store::default();
        let k = key();
        assert_eq!(
            store.restore(&k, vec![vm("a", "A"), vm("gone", "Gone")], Some(2), 1_000),
            2
        );
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a", "gone"]);
        assert_eq!((t.generation, t.total), (0, Some(2)));
        assert_eq!(t.restored_at, Some(1_000));
        assert!(!t.loading, "there are rows on screen; nothing is loading");
        assert!(t.last_poll.is_none(), "and nothing has polled");
        assert_eq!(store.names().get("a"), Some("A"), "names come back too");

        // The first live cycle replaces the rows wholesale, which is how a since-deleted row
        // disappears: no new machinery, just `Update::Complete`.
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A"), vm("b", "B")],
                total: Some(2),
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert!(t.last_poll.is_some());
        assert_eq!(
            t.restored_at, None,
            "a cycle that replaced the rows is no longer showing restored ones"
        );
    }

    /// A restored table whose first walk fails keeps its rows, keeps its age, and shows the
    /// error in the flash. The existing rule for a failing live table; the age is what makes
    /// it readable.
    #[test]
    fn a_failed_first_cycle_keeps_the_restored_rows_and_the_badge() {
        let mut store = Store::default();
        let k = key();
        assert_eq!(store.restore(&k, vec![vm("a", "A")], Some(1), 1_000), 1);
        store.apply(
            &k,
            Update::Error {
                generation: 1,
                error: failed("boom"),
            },
        );
        let t = store.table(&k);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.restored_at, Some(1_000));
        assert_eq!(t.error.as_ref().map(Failure::text).as_deref(), Some("boom"));
    }

    /// A row with no ext id has no identity, no `IndexMap` key and no way to be drilled into.
    #[test]
    fn a_row_without_an_ext_id_is_not_restored() {
        let mut store = Store::default();
        let k = key();
        assert_eq!(
            store.restore(&k, vec![vm("", "nameless"), vm("a", "A")], None, 1_000),
            1
        );
        assert_eq!(store.table(&k).rows.keys().collect::<Vec<_>>(), ["a"]);
    }

    /// A restore that painted nothing is not a restore: an ext-id key curated since the write
    /// drops every cached row, and the table must read `○ syncing` rather than being badged
    /// `◌ cached` with nothing in it.
    #[test]
    fn a_restore_of_nothing_but_nameless_rows_leaves_the_table_syncing() {
        let mut store = Store::default();
        let k = key();
        assert_eq!(
            store.restore(&k, vec![vm("", "nameless")], Some(1), 1_000),
            0
        );
        let t = store.table(&k);
        assert!(t.rows.is_empty());
        assert!(t.loading, "nothing was painted; it is still syncing");
        assert_eq!(t.restored_at, None, "and nothing is the cache's to badge");
        assert_eq!(store.tables().count(), 0, "no phantom entry either");
    }

    /// A restore is a head start, never a rewind. On a table that has already completed a live
    /// cycle it does nothing at all: rewinding the generation to 0 would re-admit a superseded
    /// cycle's pages, and stamping `restored_at` would badge live rows as the cache's.
    #[test]
    fn a_restore_onto_a_table_that_has_polled_changes_nothing() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: Some(1),
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });

        assert_eq!(store.restore(&k, vec![vm("b", "B")], Some(9), 1_000), 0);
        let t = store.table(&k);
        assert_eq!(t.rows.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!((t.generation, t.total), (1, Some(1)));
        assert_eq!(t.restored_at, None);
    }

    /// `restored_at` is cleared by the cycle that *replaces* the rows, not by any completion:
    /// a single-entity `Complete` stages nothing and leaves the restored rows where they are,
    /// so it leaves the badge that explains them there too - even though the table has polled.
    #[test]
    fn a_single_entity_complete_keeps_the_restored_badge() {
        let mut store = Store::default();
        let k = key();
        assert_eq!(store.restore(&k, vec![vm("a", "A")], Some(1), 1_000), 1);
        store.apply(
            &k,
            Update::Entity {
                generation: 3,
                entity: vm("a", "A2"),
            },
        );
        store.apply(&k, Update::Complete { generation: 3 });
        let t = store.table(&k);
        assert!(t.last_poll.is_some(), "it has polled");
        assert_eq!(
            t.restored_at,
            Some(1_000),
            "but nothing replaced the rows, so they are still the cache's"
        );
        assert_eq!(t.rows.len(), 1);
    }

    /// `Started` anchors the clock the title's elapsed time is measured from, and carries its own
    /// generation because `Table::generation` is the last *completed* cycle and nothing else
    /// records the in-flight one: with a bare `Option<Instant>` a late `Complete` from an
    /// overtaken cycle would blank a newer cycle's clock mid-walk.
    #[test]
    fn a_started_anchors_the_cycle_and_only_its_own_completion_clears_it() {
        let mut store = Store::default();
        let k = key();
        store.apply(&k, Update::Started { generation: 1 });
        let started = store.table(&k).cycle_started;
        assert_eq!(
            started.map(|(g, _)| g),
            Some(1),
            "the clock is anchored to the cycle that started"
        );
        assert_eq!(store.table(&k).staged(), 0, "Started creates no staging");
        assert_eq!(store.table(&k).staged_total(), None);

        // A stale `Started` is ignored, exactly as a stale `Page` is.
        store.apply(&k, Update::Started { generation: 0 });
        assert_eq!(store.table(&k).cycle_started.map(|(g, _)| g), Some(1));

        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: Some(181_825),
            },
        );
        let t = store.table(&k);
        assert_eq!((t.staged(), t.staged_total()), (1, Some(181_825)));
        assert!(t.rows.is_empty(), "staged, not visible yet");

        // A newer cycle takes the clock; the older one's completion must not blank it.
        store.apply(&k, Update::Started { generation: 2 });
        assert_eq!(store.table(&k).cycle_started.map(|(g, _)| g), Some(2));
        store.apply(&k, Update::Complete { generation: 1 });
        assert_eq!(
            store.table(&k).cycle_started.map(|(g, _)| g),
            Some(2),
            "a late Complete from generation 1 does not clear generation 2's clock"
        );
        // Nor does a cycle beside the walk that never staged anything: `App::refresh_row`
        // subscribes a `once` on the walking table's own key after an action, and every
        // subscription draws from one generation counter, so that `once` always completes with
        // a higher generation than the walk it overlaps.
        store.apply(&k, Update::Complete { generation: 3 });
        assert_eq!(
            store.table(&k).cycle_started.map(|(g, _)| g),
            Some(2),
            "a `once` refresh completing beside the walk does not clear the walk's clock"
        );
        store.apply(&k, Update::Complete { generation: 2 });
        assert_eq!(store.table(&k).cycle_started, None);
    }

    /// A `Started` alone never replaces rows: `Complete` replaces them from staging, and staging
    /// still appears with the first `Page`, so a single-entity cycle - which never stages -
    /// cannot wipe a table.
    #[test]
    fn a_started_alone_never_replaces_rows() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: Some(1),
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        store.apply(&k, Update::Started { generation: 2 });
        store.apply(&k, Update::Complete { generation: 2 });
        assert_eq!(store.table(&k).rows.keys().collect::<Vec<_>>(), ["a"]);
    }

    /// An `Error` clears the clock of the cycle it belongs to, so a failed walk does not leave a
    /// second bracket ticking on the title forever.
    #[test]
    fn an_error_clears_its_own_cycles_clock() {
        let mut store = Store::default();
        let k = key();
        store.apply(&k, Update::Started { generation: 1 });
        // A failure beside the walk is not the walk's failure: a `once` refresh on the same key
        // carries a higher generation, and its error must leave the walk's clock alone.
        store.apply(
            &k,
            Update::Error {
                generation: 2,
                error: failed("the once failed"),
            },
        );
        assert_eq!(
            store.table(&k).cycle_started.map(|(g, _)| g),
            Some(1),
            "a `once` refresh failing beside the walk does not clear the walk's clock"
        );
        store.apply(
            &k,
            Update::Error {
                generation: 1,
                error: failed("boom"),
            },
        );
        assert_eq!(store.table(&k).cycle_started, None);
    }

    /// A walk that is abandoned rather than finished leaves nothing behind for the next visit:
    /// `App::pop` and `drain_stack` abort the subscription, so the `Complete` that would have
    /// cleared the clock never arrives, and a clock left ticking would make an idle table say
    /// `listing…` while the abandoned walk's total was still counted as progress.
    #[test]
    fn an_abandoned_cycle_leaves_no_clock_and_no_staged_pages() {
        let mut store = Store::default();
        let k = key();
        store.apply(
            &k,
            Update::Page {
                generation: 1,
                entities: vec![vm("a", "A")],
                total: Some(1),
            },
        );
        store.apply(&k, Update::Complete { generation: 1 });
        store.apply(&k, Update::Started { generation: 2 });
        store.apply(
            &k,
            Update::Page {
                generation: 2,
                entities: vec![vm("b", "B")],
                total: Some(181_825),
            },
        );

        store.abandon(&k);

        let t = store.table(&k);
        assert_eq!(t.cycle_started, None, "no clock is left for the next visit");
        assert_eq!(
            (t.staged(), t.staged_total()),
            (0, None),
            "the cut-short walk's pages are not progress any more"
        );
        assert_eq!(
            t.rows.keys().collect::<Vec<_>>(),
            ["a"],
            "the rows the next visit redraws from stay"
        );
    }

    /// The cache writer needs every table, and `Store` is the only thing that has them.
    #[test]
    fn every_table_is_reachable_for_the_writer() {
        let mut store = Store::default();
        let k = key();
        assert_eq!(store.restore(&k, vec![vm("a", "A")], Some(1), 1_000), 1);
        let seen: Vec<&str> = store.tables().map(|(key, _)| key.kind.id).collect();
        assert_eq!(seen, ["vmm.ahv.config.Vm"]);
    }
}
