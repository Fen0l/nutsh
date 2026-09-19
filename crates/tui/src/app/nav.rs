//! The sidebar, the menu, and the stack of views.

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
        push_view(live, kind, Vec::new(), None, None);
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
        let fed = matches!(
            page.def.summary,
            Some(crate::page::summary::DISASTER_RECOVERY | crate::page::summary::ATTENTION)
        );
        if !fed || page.sampling() {
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
                live.unsubscribe_any(view.sub);
                live.release_peer_subs(&view.peer_subs);
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
