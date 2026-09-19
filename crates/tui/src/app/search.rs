//! The `:search` screen: asking the Prism Central, and filling in as it answers.

use super::*;

impl App {
    /// `:search <term>`: every kind already loaded, looked at for one term, and **no requests**.
    ///
    /// The store is the corpus - this session's polls plus whatever the cache painted in before
    /// the first frame - which is a deliberate reach and not a shortcut: going and listing the
    /// two hundred and forty-eight kinds nobody has opened would be a thousand requests and a
    /// minute of waiting for a question the user expected to be instant. What makes it honest
    /// is that the box says so, in numbers, on every result and on none.
    pub(super) fn search_command(&mut self, term: &str) {
        let Some(live) = self.live.as_mut() else {
            self.status = Some("not connected".into());
            return;
        };
        if term.is_empty() {
            self.status = Some("search: type what to look for".into());
            return;
        }
        let query = nutsh_core::search::Query::new(term);
        let found = nutsh_core::search::across(&live.store, &query);
        let mut view = crate::search::Search::new(term.to_string(), found);

        // Ask the Prism Central for the rest. One cycle per kind, not a schedule: a search is a
        // question asked once, and the rate limiter paces the fan-out the same as everything
        // else. What lands goes in the store under its own filter, so the table a user has open
        // is never replaced by a filtered subset of itself.
        // Not the kinds this Prism Central has already refused: a search asks on the user's
        // behalf, so a 404 the session has already paid for counts, the way it does for a pane.
        let plan: Vec<_> = nutsh_core::search::plan(&query)
            .into_iter()
            .filter(|ask| pane_reason(&live.session, ask.kind).is_none())
            .collect();
        for ask in &plan {
            let key = nutsh_core::store::TableKey::filtered(
                ask.kind,
                ask.filter.as_deref().map(std::sync::Arc::from),
            );
            let mut sub = nutsh_core::scheduler::Subscription::once_list(key);
            sub.filter = ask.filter.clone();
            view.subs.push(live.scheduler.subscribe(sub));
        }
        view.asked = plan.len();

        self.search = Some(view);
        self.search_from = self.mode;
        self.mode = Mode::Search;
    }

    /// A kind the running search asked about has answered: count it and walk the store again,
    /// so results appear as they land rather than all at the end.
    pub(super) fn search_answered(&mut self, sub: nutsh_core::scheduler::SubId) {
        let Some(view) = self.search.as_mut() else {
            return;
        };
        if !view.subs.contains(&sub) {
            return;
        }
        view.answered += 1;
        let query = nutsh_core::search::Query::new(&view.term);
        if let Some(live) = self.live.as_ref() {
            let found = nutsh_core::search::across(&live.store, &query);
            view.refill(found);
        }
        self.dirty = true;
    }

    pub(super) fn handle_search(&mut self, key: Key) {
        let Some(view) = self.search.as_mut() else {
            return;
        };
        match view.key(key) {
            crate::search::Action::Moved | crate::search::Action::Ignored => {}
            crate::search::Action::Closed => {
                self.search = None;
                self.mode = self.search_from;
            }
            crate::search::Action::Chose { kind, ext_id } => {
                self.search = None;
                self.mode = self.search_from;
                self.open_hit(kind, &ext_id);
            }
        }
    }

    /// `⏎` on a result: the kind's own table, with the cursor on the row.
    ///
    /// The table and not the detail pane, because "find this VM" ends with the VM in front of
    /// you and every key that acts on one within reach. A kind that cannot be a root - a disk,
    /// a NIC - is refused in the words `open_root` already refuses it in: *open Disks from a
    /// row of Virtual Machines*, which is where it lives.
    ///
    /// The row may not be in the kind's top-level table: the hit could have come from a child
    /// table or from a page pane's filtered one. Then the cursor stays at the top and the walk
    /// `open_root` just started brings the row in. Nothing is fetched to look it up.
    pub(super) fn open_hit(&mut self, kind: &'static Kind, ext_id: &str) {
        if let Open::Greyed(reason) = self.open_root(kind) {
            self.status = Some(reason);
            return;
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let Some(view) = live.table() else {
            return;
        };
        let merged = live.merged(&view.key);
        let columns = crate::table::columns_for(view.key.kind, view.wide, self.merges(&view.key));
        let at = crate::table::order(
            &merged,
            &columns,
            live.store.names(),
            self.now,
            view.sort,
            None,
        )
        .iter()
        .position(|id| *id == ext_id);
        if let (Some(at), Some(view)) = (at, self.live.as_mut().and_then(Live::table_mut)) {
            view.selected = at;
        }
    }
}
