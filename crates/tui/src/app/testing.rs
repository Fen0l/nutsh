//! What the integration tests reach for and nothing else may: injected rows, a drained
//! channel, and the accessors. One gate on the module rather than one per method.

use super::*;

impl App {
    /// Tests only: the name of the row under the cursor, which is what a test asserts on and
    /// what a frame already shows.
    pub fn selected_name(&self) -> Option<&str> {
        let live = self.live.as_ref()?;
        let ext_id = self.selected_ext_id()?;
        let table = live.store.table(self.cursor_table()?.key);
        table.rows.get(ext_id).map(|e| e.name.as_str())
    }

    /// Tests only: the last cycle failed and the values are the previous ones.
    pub fn inject_stale_stats(&mut self) {
        if let Some(live) = self.live.as_mut() {
            let stats = nutsh_core::stats::Stats {
                stale: true,
                ..*live.store.stats()
            };
            live.store.set_stats(stats);
            self.dirty = true;
        }
    }

    /// Tests only: apply whatever is already in the poll channel, and wait for nothing. What
    /// "two more turns of the event loop" means to a test whose whole point is that nothing
    /// should arrive - `settle_once` would wait forever for a cycle that is never coming.
    pub async fn drain(&mut self) {
        while let Ok(msg) = self.poll_rx.try_recv() {
            self.apply(msg);
        }
        tokio::task::yield_now().await;
    }

    /// Tests only: mark the current table's last cycle as failed.
    pub fn inject_error(&mut self, error: &str) {
        if let Some(live) = self.live.as_mut()
            && let Some(key) = live.table().map(|view| view.key.clone())
        {
            let generation = live.store.table(&key).generation + 1;
            live.store.apply(
                &key,
                nutsh_core::store::Update::Error {
                    generation,
                    error: nutsh_core::store::Failure::local(error),
                },
            );
            self.dirty = true;
        }
    }

    /// Tests only: replace the current table with `n` synthetic rows named `vm-000` upwards,
    /// so a test can reach a table taller than any terminal without a fixture that size.
    pub fn inject_rows(&mut self, n: usize) {
        self.inject_entities(
            (0..n)
                .map(|i| {
                    let name = format!("vm-{i:03}");
                    serde_json::json!({ "extId": name, "name": name })
                })
                .collect(),
        );
    }

    /// Tests only: a listing cycle in flight on the current table, `staged` rows already staged
    /// out of `total`, begun `ago` ago.
    ///
    /// The elapsed time is asserted by moving the *start*, not by freezing a clock: `App::now` is
    /// a `SystemTime` and a cycle's clock is an `Instant`, and the two cannot be the same one - a
    /// `SystemTime` cannot measure an elapsed time and an `Instant` cannot be pinned to a date.
    /// `cell::span` truncates to whole seconds, so `· 3s` is stable for any test that does not
    /// itself take a second.
    pub fn inject_walking(&mut self, staged: usize, total: u64, ago: std::time::Duration) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(key) = live.table().map(|view| view.key.clone()) else {
            return;
        };
        let kind = key.kind;
        let generation = live.store.table(&key).generation + 1;
        live.store
            .apply(&key, nutsh_core::store::Update::Started { generation });
        // Unconditionally, `staged` of 0 included: an empty first page carrying the server's
        // total is exactly what a large collection answers with, and it is the only way
        // `[0/181825]` - the frame this phase exists to produce - is reachable at all.
        let entities = (0..staged)
            .map(|i| {
                let name = format!("staged-{i:04}");
                nutsh_prism::Entity::new(
                    kind,
                    serde_json::json!({ "extId": name, "name": name }),
                    None,
                )
            })
            .collect();
        live.store.apply(
            &key,
            nutsh_core::store::Update::Page {
                generation,
                entities,
                total: Some(total),
            },
        );
        // After the `Page`, because `Update::Page` does not touch the clock and
        // `Update::Started` set it to *now*: the backdated value is what the frame must read.
        let started = std::time::Instant::now()
            .checked_sub(ago)
            .expect("the process has been up longer than `ago`");
        live.store.set_cycle_started(&key, (generation, started));
        self.dirty = true;
    }

    /// Tests only: replace the current table with these raw entities, so a style test can put
    /// one row per status role on screen without a fixture per role.
    pub fn inject_entities(&mut self, raw: Vec<serde_json::Value>) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(key) = live.table().map(|view| view.key.clone()) else {
            return;
        };
        let kind = key.kind;
        let total = Some(u64::try_from(raw.len()).unwrap_or(u64::MAX));
        let entities = raw
            .into_iter()
            .map(|r| nutsh_prism::Entity::new(kind, r, None))
            .collect();
        let generation = live.store.table(&key).generation + 1;
        live.store.apply(
            &key,
            nutsh_core::store::Update::Page {
                generation,
                entities,
                total,
            },
        );
        live.store
            .apply(&key, nutsh_core::store::Update::Complete { generation });
        live.clamp_selection();
        self.dirty = true;
    }

    /// Tests only: the extIds `space` has marked, sorted so an assertion is stable.
    pub fn marks(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .view()
            .map(|v| v.marks.iter().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Tests only: the catalog name of the action under the menu's cursor.
    pub fn menu_selected_action(&self) -> Option<&'static str> {
        Some(self.menu.as_ref()?.current()?.action.name)
    }

    /// Tests only: which header the next frame draws, without a config file to write it to.
    /// The frame tests measure the two heights against each other, and going through
    /// `:header` would also exercise the palette and the contexts seam.
    pub fn set_header(&mut self, header: Header) {
        self.header = header;
    }

    /// Tests only: the catalog names the menu is listing, in the order it lists them. The
    /// frame shows titles, and a title says nothing about which catalog action produced it.
    pub fn menu_actions(&self) -> Vec<&'static str> {
        self.menu
            .as_ref()
            .map(|m| m.entries.iter().map(|r| r.action.name).collect())
            .unwrap_or_default()
    }

    /// Tests only: the catalog names in the open picker's actions group, in the order it lists
    /// them. Together with [`App::menu_actions`] and [`App::detail_action_titles`] this is how
    /// "one list, three surfaces" is asserted rather than asserted about.
    pub fn picker_actions(&self) -> Vec<&'static str> {
        self.picker
            .as_ref()
            .map(|p| {
                p.entries
                    .iter()
                    .filter_map(|e| match e {
                        crate::picker::Entry::Act { action, .. } => Some(action.name),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Tests only: the labels the open detail pane lists under its `ACTIONS` heading, in the
    /// order it lists them. Titles rather than names, because the drawn lines are what this
    /// surface has: it renders no catalog name anywhere.
    pub fn detail_action_titles(&self) -> Vec<&'static str> {
        self.detail_actions()
            .iter()
            .map(|r| r.action.title())
            .collect()
    }

    /// Tests only: drains poll messages until the open form's reference picker has rows. It
    /// opens on what the store holds and is refilled by the one-shot list it subscribed, and
    /// no ordinary settle waits for that: the list belongs to no view.
    ///
    /// Like [`App::settle_once`], it waits forever if the rows never arrive: every caller
    /// wraps it in a timeout.
    pub async fn settle_ref_rows(&mut self) {
        if self.form.as_ref().and_then(|f| f.picker.as_ref()).is_none() {
            // Nothing opened the picker, so nothing will ever fill it. Returning says so where
            // the caller's assertion is; waiting would report it as the caller's timeout.
            return;
        }
        while self
            .form
            .as_ref()
            .and_then(|f| f.picker.as_ref())
            .is_none_or(|p| p.entries.is_empty())
        {
            let Some(msg) = self.poll_rx.recv().await else {
                return;
            };
            self.apply(msg);
        }
    }

    /// Drains the poll channel until the first `Acted`.
    ///
    /// Like [`App::settle_once`], it waits forever if that message never arrives: every caller
    /// wraps it in a timeout.
    pub async fn settle_acted(&mut self) {
        self.settle_until(|msg| matches!(msg, Msg::Acted { .. }))
            .await;
    }

    /// Tests only: drains poll messages until every kind the open search asked has answered.
    /// Returns at once when no search is open or nothing was asked.
    pub async fn settle_search(&mut self) {
        while self.search.as_ref().is_some_and(|s| s.answered < s.asked) {
            let Some(msg) = self.poll_rx.recv().await else {
                return;
            };
            self.apply(msg);
        }
    }

    /// Tests only: how many kinds the open search asked, and how many have answered.
    pub fn search_reach_for_test(&self) -> Option<(usize, usize)> {
        self.search.as_ref().map(|s| (s.asked, s.answered))
    }

    /// Tests only: waits for the next connect result and applies it.
    ///
    /// The event loop cannot call this, which is why it is not on its path: it selects over the
    /// connect channel alongside the poll channel and the terminal's events, and a method that
    /// borrows the whole app cannot sit in one arm of that `select!` while another arm borrows
    /// the poll channel out of the same app.
    pub async fn await_connect(&mut self) {
        if let Some(result) = self.connect_rx.recv().await {
            self.connected(result);
        }
    }

    /// Tests only: the hit map the last `frame` recorded.
    pub fn hits_for_test(&self) -> std::cell::Ref<'_, crate::mouse::Hits> {
        self.hits.borrow()
    }

    /// Tests only: the current table's sort, as `(index into the full column list, reversed)`.
    pub fn sort_for_test(&self) -> Option<(usize, bool)> {
        self.live.as_ref()?.table()?.sort
    }

    /// Tests only: the palette's selected entry.
    pub fn palette_selected_for_test(&self) -> usize {
        self.palette.as_ref().map_or(0, |p| p.selected)
    }

    /// Tests only: which pane of the open page has the cursor.
    pub fn focused_pane_for_test(&self) -> Option<usize> {
        Some(self.page()?.focus)
    }

    /// Tests only: what the event loop does after it draws.
    pub fn clear_dirty_for_test(&mut self) {
        self.dirty = false;
    }
}
