//! Moving around: the sidebar, tables, pages, filters and marks.

use super::*;

impl App {
    /// Close the palette, the picker and the detail pane, unsubscribing what the detail
    /// polls. A modal belongs to the view under it: when that view is replaced - a new
    /// session, a new root table - a detail left open would go on polling, and its
    /// subscription id could be handed to whichever view came next.
    pub(crate) fn close_modals(&mut self) {
        self.palette = None;
        self.picker = None;
        self.skins = None;
        self.settings = None;
        self.menu = None;
        self.confirm = None;
        self.form = None;
        self.journal_view = None;
        self.search = None;
        self.pending = None;
        self.close_detail();
    }

    /// What the menu greys, and what it drops.
    ///
    /// Greying is unconditional and is computed with `unavailable_reason` - the *same function*
    /// the palette greys an entry with and `open_root` refuses with, so the three surfaces cannot
    /// drift apart. Dropping is rule 1 always, and rule 2 only when `[nav] hide_unserved` is set
    /// and `:all` is off: nothing is hidden unless the user asked for it.
    ///
    /// Event-driven, never per frame: recomputed at connect, on `:hide`/`:show`, on `:all` and on
    /// a context switch, which is exactly when the facts behind it can change. Negotiation has
    /// answered before a `Session` exists, so nothing is greyed on a guess - and with no session
    /// at all nothing is greyed, because an unknown answer hides nothing, ever.
    ///
    /// The walk is over `KINDS` rather than over `statuses()` on purpose: 262 map lookups once
    /// per connect is nothing, and reading the answer out of the one function the other two
    /// surfaces read it out of is the whole point.
    pub(crate) fn apply_nav(&mut self) {
        let mut unserved: HashMap<&'static str, String> = HashMap::new();
        if let Some(live) = self.live.as_ref() {
            for kind in nutsh_catalog::KINDS.iter() {
                if let Some(reason) = unavailable_reason(&live.session, kind) {
                    unserved.insert(kind.id, reason);
                }
            }
        }
        let mut hidden: HashSet<&'static str> = HashSet::new();
        // Rule 1, first and unconditionally: an id in `[nav] hide` is hidden even when its
        // namespace is served, because the user said so.
        for entry in &self.nav_hide {
            if let Some(id) = nav_id(entry) {
                hidden.insert(id);
            }
        }
        // Rule 2, only when asked for: the greyed kinds leave the menu instead of staying dim.
        if self.nav_hide_unserved && !self.nav_all {
            hidden.extend(unserved.keys().copied());
        }
        // The Dashboard is the way back to a screen that works, so it is never hidden - under
        // either of the two names that reach this set. `:hide` refuses both up front; this is
        // the same rule applied to a config file someone edited by hand.
        hidden.retain(|id| !crate::sidebar::protected(id));
        self.sidebar.set_unserved(unserved);
        self.sidebar.set_hidden(hidden);
        self.dirty = true;
    }

    /// `:hide <id|group>` adds an entry and `:show <id>` removes one; both persist through the
    /// same seam `:skin` writes through, so the file's other sections do not move. A failed write
    /// is a status line, not a refusal: the menu already changed, and telling the user it did not
    /// happen would be a lie.
    pub(super) fn nav_command(&mut self, argument: Option<String>, hide: bool) {
        let word = if hide { ":hide" } else { ":show" };
        let Some(name) = argument.filter(|a| !a.trim().is_empty()) else {
            self.status = Some(format!("{word}: name a kind id, a page id or a group"));
            return;
        };
        let Some(id) = nav_id(name.trim()) else {
            self.status = Some(format!("{name}: no such kind, page or group"));
            return;
        };
        // Refused where it is asked, not silently cancelled where it is applied: saying `hidden
        // Dashboard` and writing an entry that `apply_nav` then ignores is two lies about one
        // instruction, and the dead entry outlives the session.
        if hide && crate::sidebar::protected(id) {
            self.status = Some("the Dashboard cannot be hidden".into());
            return;
        }
        self.nav_hide.retain(|e| nav_id(e) != Some(id));
        if hide {
            self.nav_hide.push(id.to_string());
        }
        self.apply_nav();
        let done = if hide { "hidden" } else { "shown" };
        self.status = Some(match self.contexts.set_nav_hidden(&self.nav_hide) {
            Ok(()) => format!("{done} {id}"),
            Err(e) => format!("{done} {id}; not saved: {e:#}"),
        });
    }

    /// Open `kind` as the root table, dropping the stack and every modal over it. Greyed
    /// kinds explain instead, leaving what is on screen alone.
    pub fn open_root(&mut self, kind: &'static Kind) -> Open {
        if let Some(reason) = reach(kind).reason() {
            return Open::Greyed(reason);
        }
        let Some(live) = self.live.as_ref() else {
            return Open::Greyed("not connected".into());
        };
        if let Some(reason) = unavailable_reason(&live.session, kind) {
            return Open::Greyed(reason);
        }
        self.close_modals();
        let live = self.live.as_mut().expect("a session, checked just above");
        drain_stack(live);
        push_view(live, kind, Vec::new(), None);
        self.mode = Mode::Table;
        self.dirty = true;
        // The menu marks whatever the body shows, however it was opened: a palette jump and a
        // menu `enter` must leave the same row highlighted. Scrolling it into view is `draw`'s
        // job, which frames the selection against the pane it actually has.
        self.sidebar
            .reveal(&nutsh_catalog::NavTarget::Kind(kind.id));
        Open::Opened
    }

    /// Open a feature page as the root view, dropping the stack. A page has no `reach` and no
    /// availability of its own: a pane the session cannot serve says so inside its own block,
    /// which is why the page opens even when every namespace it names is missing.
    pub fn open_page(&mut self, def: &'static PageDef) -> Open {
        if self.live.is_none() {
            return Open::Greyed("not connected".into());
        }
        self.close_modals();
        let live = self.live.as_mut().expect("a session, checked just above");
        drain_stack(live);
        let Live {
            session, scheduler, ..
        } = live;
        let page = PageView::open(def, scheduler, |kind| pane_reason(session, kind));
        live.stack.push(View::Page(page));
        // The invariant `hint_text` rests on, pinned where it is made rather than where it is
        // read: a page is opened onto a drained stack and is therefore always its own root, so
        // `App::back` on a page can only be rule 6 and the prompt line can say `esc:menu`
        // without asking the stack. A future push of a page over a table breaks the hint, and
        // this is what tells whoever writes it.
        debug_assert!(
            matches!(live.stack.as_slice(), [View::Page(_)]),
            "a page is always the root of its stack"
        );
        live.refresh_summary();
        self.start_sampler();
        self.mode = Mode::Table;
        self.dirty = true;
        // As `open_root` does, and for the same reason: the menu marks whatever the body
        // shows, however it was opened - the menu, the palette, or the command line - and a
        // page that revealed nothing would leave the `Kind:` field empty, since the
        // breadcrumb is the menu's path to what is open.
        self.sidebar.reveal(&nutsh_catalog::NavTarget::Page(def.id));
        Open::Opened
    }

    /// Start the open page's own poller, for the page that declares the summary it feeds.
    ///
    /// It is not a subscription - the scheduler knows nothing about it - so it is started here
    /// and stopped by `PageView::release`, wherever the panes are subscribed and released:
    /// `open_page` and `pop`. Idempotent, so a pop onto a page that never buried its sampler
    /// does not start a second one.
    pub(super) fn start_sampler(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Live {
            session,
            stack,
            scheduler,
            ..
        } = live;
        let Some(View::Page(page)) = stack.last_mut() else {
            return;
        };
        if page.def.summary != Some(crate::page::summary::DISASTER_RECOVERY) || page.sampling() {
            return;
        }
        page.sampler = Some(nutsh_core::sampler::spawn(
            session.client.clone(),
            self.poll_tx.clone(),
            self.session_generation,
            // Fifty-odd gets a cycle, for a block nobody is looking at: the sampler is not a
            // `Subscription`, so it consults the scheduler's flag itself.
            scheduler.idle_flag(),
        ));
    }

    pub(super) fn sidebar_action(&mut self, action: crate::sidebar::Action) {
        match action {
            crate::sidebar::Action::Moved
            | crate::sidebar::Action::Filtered
            | crate::sidebar::Action::Toggled
            | crate::sidebar::Action::Ignored => {}
            crate::sidebar::Action::Blur => self.focus = Focus::Body,
            // The menu binds a dozen keys; the rest still belong to the body, which is what
            // the prompt line under the menu goes on advertising.
            crate::sidebar::Action::Passthrough(key) => self.handle_table(key),
            crate::sidebar::Action::Open(item) => self.open_target(item),
        }
    }

    /// What a menu item opens. The keys go back to the body either way: the user asked for a
    /// table, not for another turn in the menu.
    ///
    /// The item travels whole rather than as a label: two groups may carry the same label -
    /// the curated `VMs` and the generated `vmm` one both do - so its `note` cannot be looked
    /// up afterwards without risking the wrong item's.
    pub(crate) fn open_target(&mut self, item: &'static nutsh_catalog::NavItem) {
        let (target, label) = (item.target, item.label);
        self.focus = Focus::Body;
        match target {
            nutsh_catalog::NavTarget::Kind(id) => {
                let Some(kind) = nutsh_catalog::kind(id) else {
                    self.status = Some(format!("{label}: not in the catalog"));
                    return;
                };
                // `open_root` reveals what it opened, so the marker follows on its own.
                if let Open::Greyed(reason) = self.open_root(kind) {
                    self.status = Some(format!("{label}: {reason}"));
                }
            }
            nutsh_catalog::NavTarget::Page(id) => {
                let Some(def) = nutsh_catalog::page(id) else {
                    self.status = Some(format!("{label}: not in the catalog"));
                    return;
                };
                // `open_page` reveals what it opened, so the marker follows on its own.
                if let Open::Greyed(reason) = self.open_page(def) {
                    self.status = Some(format!("{label}: {reason}"));
                }
            }
            nutsh_catalog::NavTarget::Contexts => {
                self.sidebar.reveal(&target);
                self.show_contexts(None);
            }
            nutsh_catalog::NavTarget::Settings => {
                self.sidebar.reveal(&target);
                self.show_settings();
            }
            nutsh_catalog::NavTarget::Missing => {
                let note = item.note.unwrap_or("not available");
                self.status = Some(format!("{label}: {note}"));
            }
        }
    }

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
        let Some(ext_id) = self.selected_ext_id().map(str::to_string) else {
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

    /// What `esc` does in the body, in precedence order (§7.1). The first rule that matches
    /// wins, and the rules above these are handled before it is reached: a modal closes
    /// itself, and the sidebar's `/` filter and its blur are `Sidebar::key`'s.
    pub(super) fn back(&mut self) {
        // Rule 4: marks on the current view, cleared instead of popping, so `esc` never
        // surprises.
        if self.clear_marks() {
            return;
        }
        // Rule 4b: a standing `/` term. After the marks and before the pop, so `esc` unwinds in
        // the order the user built: mark a row inside a filtered view and the marks go first,
        // then the filter, then the view.
        if self.clear_filter() {
            return;
        }
        // Rule 5: a view to pop.
        if !self.at_root() {
            self.pop();
            return;
        }
        // Rule 6: a root table or a page. `esc` says "take me back", and from the root the
        // only place back is the menu.
        self.focus_menu();
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

    /// Show the menu if it is hidden, and move the focus to it. The digits do exactly this
    /// (`Key::Char(c) if c.is_ascii_digit()`), and for the same reason: a key that advertises
    /// "take me to the menu" must not be a no-op at the width where the menu is not drawn.
    /// One helper, so the two paths cannot diverge.
    pub(crate) fn focus_menu(&mut self) {
        self.sidebar.visible = true;
        self.sidebar.toggled = true;
        self.focus_sidebar();
    }

    /// Hand the keys to the menu. The `/` term stops taking them and stays standing, which is
    /// the one rule that keeps `App::filtering` honest: the term has the keys exactly while the
    /// body does, so a digit pressed in the menu is still a jump to group N, and the prompt
    /// line under a focused menu is the body's hints rather than a cursor nobody is typing at.
    pub(crate) fn focus_sidebar(&mut self) {
        if let Some(filter) = self
            .live
            .as_mut()
            .and_then(Live::table_mut)
            .and_then(|v| v.filter.as_mut())
        {
            filter.commit();
        }
        self.focus = Focus::Sidebar;
    }

    /// Whether the body shows the root of its stack: what `esc` acts on, and what the prompt
    /// line's hint says it will do.
    pub(crate) fn at_root(&self) -> bool {
        self.live.as_ref().is_none_or(|l| l.stack.len() <= 1)
    }

    /// The keys of a page: `tab` between panes, `O` for the focused pane's kind as a full
    /// table, and the cursor keys inside the focused pane.
    pub(super) fn handle_page(&mut self, key: Key) {
        match key {
            Key::Tab => self.cycle_pane(true),
            Key::BackTab => self.cycle_pane(false),
            Key::Char('O') => self.open_focused_pane(),
            Key::Enter | Key::Char('y') => self.open_detail(),
            // A pane row is a table row: the whole action pipeline reads `App::cursor_table`,
            // which answers for a focused pane as it answers for a table view, so `a` here runs
            // on the same row `⏎` would open. The Dashboard is the launch view and every group
            // opens a page, so this is where most sessions meet an action at all.
            Key::Char('a') => self.open_menu(),
            Key::Esc => self.back(),
            // A page marks nothing and acts on nothing, but `q` means the same everywhere a
            // body is: a quit that depends on which view is open is one nobody trusts.
            Key::Char('q') => self.request_quit(),
            Key::Char(':') => self.open_palette(),
            Key::Char('?') => {
                self.help_from = self.mode;
                self.mode = Mode::Help;
            }
            _ => self.move_in_pane(key),
        }
    }

    /// `tab`/`shift-tab` between the panes of the open page.
    pub(super) fn cycle_pane(&mut self, forward: bool) {
        if let Some(live) = self.live.as_mut()
            && let Some(page) = live.page_mut()
        {
            page.cycle(forward);
        }
    }

    /// The keys of a pane that only move its cursor or ask for a poll now.
    pub(super) fn move_in_pane(&mut self, key: Key) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Live { store, stack, .. } = live;
        let Some(View::Page(page)) = stack.last_mut() else {
            return;
        };
        let Some(pane) = page.focused_mut() else {
            return;
        };
        let last = store.table(&pane.key).rows.len().saturating_sub(1);
        let mut flash: Option<&'static str> = None;
        let mut retime = false;
        match key {
            Key::Char('j') | Key::Down => pane.selected = (pane.selected + 1).min(last),
            Key::Char('k') | Key::Up => pane.selected = pane.selected.saturating_sub(1),
            Key::Char('g') | Key::Home => pane.selected = 0,
            Key::Char('G') | Key::End => pane.selected = last,
            Key::PageDown => pane.selected = (pane.selected + 20).min(last),
            Key::PageUp => pane.selected = pane.selected.saturating_sub(20),
            Key::Ctrl('r') => {
                pane.stopped = false;
                if let Some(sub) = pane.sub {
                    live.scheduler.refresh(sub);
                }
            }
            Key::Ctrl('x') => {
                let stopped = pane.sub.is_some_and(|sub| live.scheduler.stop(sub));
                pane.stopped = stopped;
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

    /// `O`: the focused pane's kind as a full table, pushed over the page. The table is the
    /// unfiltered listing - the pane's filter is what made it a pane, not what the kind is.
    ///
    /// Refused for the same two reasons `open_root` refuses: a pane already saying why this
    /// Prism Central cannot serve it would otherwise open a table that polls and fails on a
    /// loop with nothing marking it, and a child kind has no listing of its own to open -
    /// `TableKey::under(kind, vec![])` would leave its parent placeholders unfilled.
    pub(super) fn open_focused_pane(&mut self) {
        let Some(pane) = self.page().and_then(|p| p.focused()) else {
            return;
        };
        let (kind, title) = (pane.key.kind, pane.def.title);
        let refused = pane.reason.clone().or_else(|| reach(kind).reason());
        if let Some(reason) = refused {
            self.status = Some(format!("{title}: {reason}"));
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        push_view(live, kind, Vec::new(), None);
    }

    /// The keys of a table that only move the cursor or ask for a poll now.
    pub(super) fn move_in_table(&mut self, key: Key) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Live {
            store,
            stack,
            scheduler,
            ..
        } = live;
        let Some(View::Table(view)) = stack.last_mut() else {
            return;
        };
        // What the frame shows, not what the table holds: `G` on a filtered table goes to the
        // last row that matches.
        let len = crate::table::matching(store.table(&view.key), view.query());
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
                let last =
                    crate::table::max_offset(crate::table::columns(view.key.kind, view.wide));
                view.col_offset = (view.col_offset + 1).min(last);
            }
            // `ctrl-r` is the way out of a stopped view as well as a request for a cycle now:
            // `Scheduler::refresh` clears the subscription's own flag, and this clears the
            // view's.
            Key::Ctrl('r') => {
                view.stopped = false;
                scheduler.refresh(view.sub);
            }
            Key::Ctrl('x') => {
                let stopped = scheduler.stop(view.sub);
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
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(view) = live.table_mut() else {
            return;
        };
        let columns = crate::table::columns(view.key.kind, view.wide).len();
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
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.table_mut()
        {
            view.wide = !view.wide;
            let last = crate::table::max_offset(crate::table::columns(view.key.kind, view.wide));
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
        let ext_id = self.selected_ext_id()?;
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        let name = live
            .store
            .table(cursor.key)
            .rows
            .get(ext_id)
            .map_or_else(|| ext_id.to_string(), |e| e.name.clone());
        Some(Picker::open(
            cursor.key.kind,
            ext_id.to_string(),
            name,
            |kind| unavailable_reason(&live.session, kind),
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
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                // The child's list path spells out the whole chain, so the row's own extId is
                // appended to the parents the current view already carries.
                let mut parents = live
                    .table()
                    .map(|v| v.key.parents.clone())
                    .unwrap_or_default();
                parents.push(picker.parent_ext_id);
                push_view(live, child, parents, Some(picker.parent_name));
            }
        }
    }

    /// The view underneath the top one, or nothing when there is only one. Rule 5 of what
    /// `esc` does, and [`App::back`] is what decides to call it - at the root it goes to the
    /// menu instead.
    ///
    /// The popped table's rows stay in the store on purpose: drilling into the same row again
    /// then shows what it held while the first poll of the new subscription runs, instead of
    /// an empty screen. An eviction policy can come once
    /// a session can wander through enough tables for their rows to add up.
    pub(super) fn pop(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if live.stack.len() <= 1 {
            return;
        }
        match live.stack.pop() {
            Some(View::Table(view)) => {
                live.scheduler.unsubscribe(view.sub);
                live.store.abandon(&view.key);
            }
            Some(View::Page(mut page)) => {
                release_page(&mut page, &mut live.scheduler, &mut live.store);
            }
            None => {}
        }
        if let Some(top) = live.stack.last_mut() {
            top.reacquire(&mut live.scheduler);
        }
        // The page's own poller went down with its panes when it was buried; it comes back
        // with them.
        self.start_sampler();
    }
}
