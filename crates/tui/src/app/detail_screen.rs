//! The composed detail pane and its payload view.

use super::*;

impl App {
    /// A pane over a document an action answered with. It arrives on the poll channel, so it
    /// can land on any frame: over an open confirm, a half-filled form, a palette mid-word, or
    /// the pane of a row being read. Every one of those is closed with it - `esc` leaves the
    /// payload for the table, and the state behind it has to be the table too - and the pane it
    /// replaces is unsubscribed.
    pub(super) fn open_payload(&mut self, title: String, value: Value) {
        self.close_modals();
        self.detail = Some(Detail::payload(title, value));
        self.mode = Mode::Detail;
    }

    /// Drop the open pane, unsubscribing what it polled. A payload pane polls nothing, so
    /// closing one unsubscribes nothing.
    pub(super) fn close_detail(&mut self) {
        if let Some(sub) = self.detail.take().and_then(|d| d.sub)
            && let Some(live) = self.live.as_mut()
        {
            live.scheduler.unsubscribe(sub);
        }
    }

    /// `y`, or `enter` on a kind with no children: the entity, refreshed by a subscription of
    /// its own on the same rhythm as the table. On a page it is the focused pane's row.
    pub(super) fn open_detail(&mut self) {
        // The pane's actions section greys on this account's roles, and a draw path holds
        // `&App` and cannot start the four list calls that answer for them. Started here, they
        // are in flight while the pane is being read.
        self.start_can_i();
        let Some(ext_id) = self.selected_ext_id().map(str::to_string) else {
            return;
        };
        let Some(key) = self.cursor_table().map(|cursor| cursor.key.clone()) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // No `get_path` means no single-entity read to issue, which the flat Host is the
        // standing example of. Subscribing one anyway spends a cycle to be told so by the
        // catalog and paints `has no get endpoint` across the top of the pane, which is a fact
        // about the catalog and not something to tell the reader in the middle of their frame.
        // The pane is composed from the list row instead, and the list is still polling.
        let sub = key.kind.get_path.map(|_| {
            live.scheduler.subscribe(Subscription::single(
                key.clone(),
                Duration::from_secs(u64::from(key.kind.poll_secs.max(1))),
                ext_id.clone(),
            ))
        });
        // A failed task's pane gathers what was around it: the alerts in the ten minutes
        // either side of its start, asked for once and read off the store.
        if let Some(task) = live.store.table(&key).rows.get(&ext_id)
            && nutsh_core::evidence::wants(task)
            && let Some(window) = nutsh_core::evidence::window(task)
            && let Some(alerts) = nutsh_catalog::kind("monitoring.serviceability.Alert")
            && pane_reason(&live.session, alerts).is_none()
        {
            let filter = nutsh_core::evidence::odata(window);
            let mut sub = Subscription::once_list(nutsh_core::store::TableKey::filtered(
                alerts,
                Some(std::sync::Arc::from(filter.as_str())),
            ));
            sub.filter = Some(filter);
            live.scheduler.subscribe(sub);
        }
        let body = detail::Body::default_for(Some(&key));
        self.detail = Some(Detail {
            key: Some(key),
            ext_id,
            sub,
            body,
            scroll: 0,
            error: None,
            payload: None,
            heading: None,
            watch: None,
        });
        self.mode = Mode::Detail;
    }

    /// `w` on the pane: poll this row at the watch rhythm and light what moves; again to stop.
    /// The rhythm is the advertised tier's, so a slow Prism Central gets a slow watch.
    pub(super) fn toggle_watch(&mut self) {
        let sections = self.detail_sections();
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let Some(sub) = detail.sub else {
            self.status = Some(format!(
                "cannot watch: {} has no single-entity read",
                detail.key.as_ref().map_or("this kind", |k| k.kind.display)
            ));
            return;
        };
        if detail.watch.take().is_some() {
            let every = detail
                .key
                .as_ref()
                .map(|k| Duration::from_secs(u64::from(k.kind.poll_secs.max(1))));
            live.scheduler.set_interval(sub, every);
            self.status = Some("watch off".into());
        } else {
            let every = nutsh_core::refresh::watch_interval(live.session.client.host_limit());
            let mut watch = crate::detail::Watch::new(every);
            watch.observe(&sections, std::time::Instant::now());
            detail.watch = Some(watch);
            live.scheduler.set_interval(sub, Some(every));
            live.scheduler.refresh(sub);
            self.status = Some(format!(
                "watching every {} · w stops",
                nutsh_core::cell::span(every.as_secs())
            ));
        }
        self.dirty = true;
    }

    /// The composed sections of the row the pane is over, as the watch compares them.
    pub(super) fn detail_sections(&self) -> Vec<nutsh_core::detail::Section> {
        let (Some(live), Some(detail), Some(entity)) = (
            self.live.as_ref(),
            self.detail.as_ref(),
            self.detail_entity(),
        ) else {
            return Vec::new();
        };
        let Some(key) = detail.key.as_ref() else {
            return Vec::new();
        };
        nutsh_core::detail::compose(key.kind, entity, live.store.names(), self.now)
    }

    pub(super) fn handle_detail(&mut self, key: Key) {
        // Taken before the pane's own keys, and for the reason the pane has an actions section
        // at all: `a` means the same here as on the table, on the row this pane is over. The
        // pane binds no `a`, so nothing is being shadowed.
        if key == Key::Char('a') {
            self.open_menu();
            return;
        }
        // Likewise: `ctrl-t` retimes the kind this pane is over, so the key means the same
        // thing wherever the body is.
        if key == Key::Ctrl('t') {
            self.step_refresh();
            return;
        }
        let last = self.detail_last_line();
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        match detail.key(key, last) {
            detail::Action::Scrolled | detail::Action::Toggled | detail::Action::Ignored => {}
            detail::Action::Watch => self.toggle_watch(),
            detail::Action::Help => {
                self.help_from = Mode::Detail;
                self.mode = Mode::Help;
            }
            detail::Action::Closed => {
                self.close_detail();
                self.mode = Mode::Table;
            }
        }
    }

    /// The last line the detail pane can scroll to, measured off the lines it would draw.
    pub(super) fn detail_last_line(&self) -> u16 {
        let Some(detail) = self.detail.as_ref() else {
            return 0;
        };
        // As `ui::draw_detail` does, and for the same reason: a payload pane reads nothing from
        // the store, so it must not need a session to measure itself.
        let empty = nutsh_core::cell::Names::default();
        let names = self.live.as_ref().map_or(&empty, |l| l.store.names());
        Detail::last_line(&detail.lines(detail::BodyView {
            entity: self.detail_entity(),
            names,
            now: self.now,
            width: self.detail_width.get(),
            actions: &self.detail_actions(),
            extra: &self.detail_extra(),
        }))
    }

    /// The evidence sections under a failed task, empty for every other pane. Read off the
    /// store each frame: the alerts land under the window's own filter key, the journal entry
    /// is the one that carries this task's id.
    pub fn detail_extra(&self) -> Vec<nutsh_core::detail::Section> {
        let (Some(live), Some(task)) = (self.live.as_ref(), self.detail_entity()) else {
            return Vec::new();
        };
        if !nutsh_core::evidence::wants(task) {
            return Vec::new();
        }
        let alerts = nutsh_core::evidence::window(task).and_then(|w| {
            let kind = nutsh_catalog::kind("monitoring.serviceability.Alert")?;
            let key = nutsh_core::store::TableKey::filtered(
                kind,
                Some(std::sync::Arc::from(
                    nutsh_core::evidence::odata(w).as_str(),
                )),
            );
            let table = live.store.table(&key);
            table
                .last_poll
                .map(|_| table.rows.values().collect::<Vec<_>>())
        });
        let journal = live
            .journal
            .entries()
            .find(|e| e.task_ext_id.as_deref() == Some(task.ext_id.as_str()));
        nutsh_core::evidence::sections(task, alerts.as_deref(), journal, self.now)
    }

    /// The actions the open pane lists under its `ACTIONS` heading: the row's, from the one
    /// list every action surface draws. Nothing for a payload pane - the document an action
    /// answered with is over no row, so there is nothing there to act on.
    ///
    /// Public because `ui::draw_detail` measures the same lines `App::detail_last_line` does,
    /// and the two must be given the same rows.
    pub fn detail_actions(&self) -> Vec<menu::Row> {
        match self.detail.as_ref() {
            Some(detail) if detail.key.is_some() => self.entity_action_rows(),
            _ => Vec::new(),
        }
    }

    /// The row an open pane is over, when it is over one: a payload pane carries its document
    /// and reads nothing from the store. Crate-internal because `ui::draw_detail` measures the
    /// same text this does, and the two must read the same row.
    pub(crate) fn detail_entity(&self) -> Option<&nutsh_prism::Entity> {
        let detail = self.detail.as_ref()?;
        let live = self.live.as_ref()?;
        // `Table::row`, not `rows`: a list narrowed by `$select` holds the columns and nothing
        // else, and the pane is composed from the whole document the detail's own single-entity
        // subscription fetched.
        live.store.table(detail.key.as_ref()?).row(&detail.ext_id)
    }
}
