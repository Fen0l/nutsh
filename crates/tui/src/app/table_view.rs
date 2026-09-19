//! A table in front of the user: filter, marks, sort, width, drilling.

use super::*;

impl App {
    pub(super) fn handle_table(&mut self, key: Key) {
        // A page is a view like a table, and the keys that reach the body reach it first: it
        // binds a smaller set, since a pane sorts and widens nothing.
        if self.page().is_some() {
            self.handle_page(key);
            return;
        }
        // Before every table key, because `j` is a letter while a term is being typed. Only
        // while it is being typed: `enter` hands the keys back and leaves the term standing.
        if self.filtering() {
            self.filter_key(key);
            return;
        }
        match key {
            Key::Char(':') => self.open_palette(),
            Key::Char('/') => self.open_filter(),
            Key::Enter => self.drill(),
            Key::Esc => self.back(),
            Key::Char(' ') => self.toggle_mark(),
            Key::Char('a') => self.open_menu(),
            Key::Char('q') => self.request_quit(),
            Key::Char('y') => self.open_detail(),
            Key::Char('S') => self.cycle_sort(),
            Key::Char('w') => self.toggle_wide(),
            Key::Char('?') => {
                self.help_from = self.mode;
                self.mode = Mode::Help;
            }
            // Nothing is hard-coded: the overlay is what makes `p`, `P`, `r`, `m`, `C`, `s`
            // and `ctrl-d` mean anything, and another kind binds its own. Resolved in the
            // fall-through rather than in a guard, so the lookup - which allocates the key's
            // name and scans the kind's actions - runs once per keypress, and the bound keys
            // above still win: `RESERVED_KEYS` is what keeps a kind from curating one of them.
            _ => match self.curated_key(key) {
                Some(action) => self.start(action),
                None => self.move_in_table(key),
            },
        }
    }

    /// Whether a table's `/` term is taking the keys. Read before the digits and before every
    /// table key, so there is one predicate and not two.
    pub(crate) fn filtering(&self) -> bool {
        self.view()
            .and_then(|v| v.filter.as_ref())
            .is_some_and(|f| f.typing())
    }

    /// `/`: an empty term with the keys in it, over the table the body is showing. Pressed
    /// again over a standing filter it re-opens the term for editing rather than clearing it,
    /// which is what `esc` is for.
    pub(super) fn open_filter(&mut self) {
        let Some(view) = self.live.as_mut().and_then(Live::table_mut) else {
            return;
        };
        match view.filter.as_mut() {
            Some(filter) => filter.reopen(),
            None => view.filter = Some(Box::new(crate::table::Filter::new())),
        }
    }

    /// One key while a term is being typed. `esc` takes the filter away, `enter` hands the keys
    /// back to the table and leaves it standing, and everything the term does not own still
    /// moves the cursor - so `↑`/`↓` walk the rows that match while they are still being
    /// narrowed.
    pub(super) fn filter_key(&mut self, key: Key) {
        let Some(view) = self.live.as_mut().and_then(Live::table_mut) else {
            return;
        };
        match key {
            Key::Esc => {
                view.filter = None;
                return;
            }
            Key::Enter => {
                if let Some(filter) = view.filter.as_mut() {
                    filter.commit();
                }
                return;
            }
            _ => {}
        }
        let owned = view
            .filter
            .as_mut()
            .and_then(|filter| filter.key(key))
            .is_some();
        if !owned {
            self.move_in_table(key);
            return;
        }
        // The rows under the term just changed; the cursor cannot stand past the end of what
        // is left. `clamp_selection` is the one place that arithmetic lives.
        if let Some(live) = self.live.as_mut() {
            live.clamp_selection();
        }
    }

    /// The kind's action with this key, in the spelling `RESERVED_KEYS` uses.
    pub(super) fn curated_key(&self, key: Key) -> Option<&'static Action> {
        let name = match key {
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("ctrl-{c}"),
            _ => return None,
        };
        let target = self.view()?.key.kind.action_target();
        target.actions.iter().find(|a| a.key == name && !a.hidden)
    }

    /// `space`: mark the row under the cursor and move down one.
    ///
    /// The set lives as long as the view, minus two ways out: `esc` clears it, and so does the
    /// action it was gathered for, once the requests are away - a mark that survived its own
    /// bulk action would silently re-run on the same rows at the next keypress.
    pub(super) fn toggle_mark(&mut self) {
        let Some(ext_id) = self.selected_ext_id() else {
            return;
        };
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.table_mut()
            && !view.marks.remove(&ext_id)
        {
            view.marks.insert(ext_id);
        }
        self.move_in_table(Key::Char('j'));
    }

    /// Drop the current table's `/` term, and say whether there was one. A filter is a way of
    /// looking at rows and not a place in the stack, so it is undone before anything is popped.
    pub(super) fn clear_filter(&mut self) -> bool {
        self.live
            .as_mut()
            .and_then(Live::table_mut)
            .is_some_and(|v| v.filter.take().is_some())
    }

    /// Drop the marks of the current table, and say whether there were any. The set does not
    /// outlive what it was gathered for.
    pub(super) fn clear_marks(&mut self) -> bool {
        self.live
            .as_mut()
            .and_then(Live::table_mut)
            .is_some_and(|v| {
                let had = !v.marks.is_empty();
                v.marks.clear();
                had
            })
    }

    /// The keys of a table that only move the cursor or ask for a poll now.
    pub(super) fn move_in_table(&mut self, key: Key) {
        let merges = self.view().is_some_and(|v| self.merges(&v.key));
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let merged = match live.stack.last() {
            Some(View::Table(v)) => live.merged(&v.key).into_owned(),
            _ => Table::default(),
        };
        let Live {
            stack,
            scheduler,
            peers,
            ..
        } = live;
        let Some(View::Table(view)) = stack.last_mut() else {
            return;
        };
        // What the frame shows, not what the table holds: `G` on a filtered table goes to the
        // last row that matches.
        let len = crate::table::matching(&merged, view.query());
        let last = len.saturating_sub(1);
        let mut flash: Option<&'static str> = None;
        let mut retime = false;
        match key {
            Key::Char('j') | Key::Down => view.selected = (view.selected + 1).min(last),
            Key::Char('k') | Key::Up => view.selected = view.selected.saturating_sub(1),
            Key::Char('g') | Key::Home => view.selected = 0,
            Key::Char('G') | Key::End => view.selected = last,
            Key::PageDown => view.selected = (view.selected + 20).min(last),
            Key::PageUp => view.selected = view.selected.saturating_sub(20),
            // `←`/`→` mean "scroll columns" in a table and "collapse/expand" in the sidebar:
            // the same shape of gesture in both, and never ambiguous, because focus decides.
            Key::Left => view.col_offset = view.col_offset.saturating_sub(1),
            Key::Right => {
                let last = crate::table::max_offset(&crate::table::columns_for(
                    view.key.kind,
                    view.wide,
                    merges,
                ));
                view.col_offset = (view.col_offset + 1).min(last);
            }
            // `ctrl-r` is the way out of a stopped view as well as a request for a cycle now:
            // `Scheduler::refresh` clears the subscription's own flag, and this clears the
            // view's.
            Key::Ctrl('r') => {
                view.stopped = false;
                // The view's own list lives on the scheduler of the context it reads.
                match view.key.context.as_deref() {
                    None => scheduler.refresh(view.sub),
                    Some(name) => {
                        if let Some(peer) = peers.iter_mut().find(|p| &*p.name == name) {
                            peer.scheduler.refresh(view.sub);
                        }
                    }
                }
                for (name, sub) in &view.peer_subs {
                    if let Some(peer) = peers.iter_mut().find(|p| p.name == *name) {
                        peer.scheduler.refresh(*sub);
                    }
                }
            }
            Key::Ctrl('x') => {
                let stopped = match view.key.context.as_deref() {
                    None => scheduler.stop(view.sub),
                    Some(name) => peers
                        .iter_mut()
                        .find(|p| &*p.name == name)
                        .is_some_and(|peer| peer.scheduler.stop(view.sub)),
                };
                for (name, sub) in &view.peer_subs {
                    if let Some(peer) = peers.iter().find(|p| p.name == *name) {
                        peer.scheduler.stop(*sub);
                    }
                }
                view.stopped = stopped;
                flash = Some(if stopped {
                    "stopped"
                } else {
                    "nothing to stop"
                });
            }
            // Handled after the borrows end, because `step_refresh` needs `self` whole.
            Key::Ctrl('t') => retime = true,
            _ => {}
        }
        // After the borrows of `live`'s fields end, because both of these need `self` again.
        if let Some(text) = flash {
            self.status = Some(text.to_string());
        }
        if retime {
            self.step_refresh();
        }
    }

    /// `S`: sort by the next column, then by that column reversed, then on to the next one;
    /// past the last column reversed, back to the order the store holds.
    pub(super) fn cycle_sort(&mut self) {
        let Some(merges) = self.view().map(|v| self.merges(&v.key)) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(view) = live.table_mut() else {
            return;
        };
        let columns = crate::table::columns_for(view.key.kind, view.wide, merges).len();
        view.sort = match view.sort {
            None => (columns > 0).then_some((0, false)),
            Some((col, false)) => Some((col, true)),
            Some((col, true)) if col + 1 < columns => Some((col + 1, false)),
            Some((_, true)) => None,
        };
    }

    /// `w`: up to twenty columns instead of six. The column offset is clamped, since `w` can
    /// narrow the column set under it.
    pub(super) fn toggle_wide(&mut self) {
        let Some(merges) = self.view().map(|v| self.merges(&v.key)) else {
            return;
        };
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.table_mut()
        {
            view.wide = !view.wide;
            let last = crate::table::max_offset(&crate::table::columns_for(
                view.key.kind,
                view.wide,
                merges,
            ));
            view.col_offset = view.col_offset.min(last);
        }
    }

    /// `enter` on a row: a picker of what can be opened under it and what can be done to it, or
    /// the detail pane when the kind has no children.
    pub(super) fn drill(&mut self) {
        // The picker greys its actions on this account's roles, and a draw cannot start the
        // four list calls that answer for them; `a` does the same for the same reason.
        self.start_can_i();
        let Some(picker) = self.build_picker() else {
            return;
        };
        if picker.is_empty() {
            self.open_detail();
            return;
        }
        self.picker = Some(picker);
        self.mode = Mode::Picker;
    }

    /// The picker for the row under the cursor, greyed by what this session serves; `None`
    /// when there is no row.
    pub(super) fn build_picker(&self) -> Option<Picker> {
        let actions = self.entity_action_rows();
        let row_id = self.selected_ext_id()?;
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        // A merged row's id names its context; the picker holds the row's own extId, and
        // greys its children by what that context serves.
        let (site, entity) = live.find_row(cursor.key, &row_id)?;
        let session = live.session_for(site.or(cursor.key.context.as_deref()));
        Some(Picker::open(
            cursor.key.kind,
            entity.ext_id.clone(),
            entity.name.clone(),
            |kind| unavailable_reason(session, kind),
            actions,
        ))
    }

    pub(super) fn handle_picker(&mut self, key: Key) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        match picker.key(key) {
            picker::Event::Filtered | picker::Event::Moved | picker::Event::Ignored => {}
            picker::Event::Cancelled => {
                self.picker = None;
                self.mode = Mode::Table;
            }
            // The same refusal the menu greys the row with, and the same path it runs down:
            // `start` composes the reason again, so the box and a direct key cannot disagree.
            picker::Event::Run(action) => {
                self.picker = None;
                self.mode = Mode::Table;
                self.start(action);
            }
            picker::Event::Chose(child) => {
                let picker = self.picker.take().expect("borrowed above");
                self.mode = Mode::Table;
                // A greyed child has nothing to open; the reason is all this key can offer.
                if let Some(reason) = picker
                    .current()
                    .and_then(|e| e.reason().map(str::to_string))
                {
                    self.status = Some(reason);
                    return;
                }
                // The child is listed from the context the row came from: the merged row's
                // id names it, and a row of a child table inherits its table's.
                let site = self
                    .selected_ext_id()
                    .and_then(|id| Live::split_row_id(&id).0.map(std::sync::Arc::from));
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                // The child's list path spells out the whole chain, so the row's own extId is
                // appended to the parents the current view already carries.
                let (mut parents, context) = live
                    .table()
                    .map(|v| (v.key.parents.clone(), v.key.context.clone()))
                    .unwrap_or_default();
                parents.push(picker.parent_ext_id);
                push_view(
                    live,
                    child,
                    parents,
                    Some(picker.parent_name),
                    site.or(context),
                );
            }
        }
    }
}
