//! Action forms and the reference picker.

use super::*;

impl App {
    /// The form's keys, or the reference picker's while one is open over it: the picker is a
    /// modal of the form, so it takes every key until it closes.
    pub(super) fn handle_fields(&mut self, key: Key) {
        if self.form.as_ref().is_some_and(|f| f.picker.is_some()) {
            self.handle_ref_picker(key);
            return;
        }
        if self.form.is_none() {
            // No form to type into is no mode to be in - and the action it was opened for goes
            // with it, for the reason `close_form` gives: a pending left behind would let the
            // next `enter` confirm something nobody is looking at.
            self.close_form();
            return;
        }
        let Some(form) = self.form.as_mut() else {
            return;
        };
        match form.key(key) {
            form::Event::Moved | form::Event::Edited | form::Event::Ignored => {}
            form::Event::Cancelled => self.close_form(),
            form::Event::Pick(kind) => self.open_ref_picker(kind),
            form::Event::Submit => self.submit_form(),
        }
    }

    /// `esc` on the form: the action goes with it, and so does the menu that chose it. A
    /// pending left behind would let the next `enter` confirm something nobody is looking at
    /// any more.
    pub(super) fn close_form(&mut self) {
        self.form = None;
        self.menu = None;
        self.pending = None;
        self.mode = Mode::Table;
    }

    /// `enter` on a `Reference` field: the rows of the kind it names. The store's rows are
    /// shown at once and a one-shot list is subscribed when no cycle has completed for that
    /// table, so the box fills in rather than the key appearing to do nothing.
    pub(super) fn open_ref_picker(&mut self, id: &'static str) {
        let Some(kind) = nutsh_catalog::kind(id) else {
            self.form_error(format!("{id} is not in the catalog"));
            return;
        };
        // A kind that needs a parent or a query parameter has no list of its own to offer;
        // saying so beats an empty box, and the extId can still be typed by hand.
        let refused = reach(kind).reason().or_else(|| {
            self.live
                .as_ref()
                .and_then(|live| unavailable_reason(&live.session, kind))
        });
        if let Some(reason) = refused {
            self.form_error(format!("{}: {reason}", kind.display));
            return;
        }
        let Some(label) = self
            .form
            .as_ref()
            .and_then(|f| f.fields.get(f.selected))
            .copied()
            .map(nutsh_core::actions::label_of)
        else {
            return;
        };
        let table_key = TableKey::top(kind);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let table = live.store.table(&table_key);
        let loading = table.loading;
        let failed = table.error.clone();
        let rows = choices(table);
        // What the store holds may be a *slice* of the collection rather than all of it, and
        // `loading` cannot say so: a page pane shares this key with the table view of its kind
        // and walks only as far as it draws (`page::subscribe`), and a kind with a catalog
        // budget stops at it. The server's `total` is the one honest measure, so the picker
        // lists again whenever it names more rows than the table has - a box offering twenty
        // of five hundred clusters with nothing on screen to say so is what this avoids.
        let partial = loading
            || table
                .total
                .is_some_and(|total| u64::try_from(table.rows.len()).is_ok_and(|had| total > had));
        // Once per kind per form: `enter`, `esc`, `enter` on the same reference would
        // otherwise put a second page walk over the same table in flight beside the first,
        // because neither `loading` nor a short table changes until the first one completes.
        let listing = partial
            && self
                .form
                .as_mut()
                .is_some_and(|form| form.first_listing_of(id));
        if listing {
            live.scheduler.subscribe(Subscription::once_list(table_key));
        }
        if let Some(form) = self.form.as_mut() {
            // Loading while a listing is in flight, whichever one it is: the table's own first
            // cycle, or the one this call just issued over rows that were a slice. Never true
            // with nothing coming, or the box would spin for ever.
            form.loading = loading || listing;
            form.listing_error = failed;
            form.picker = Some(RefPicker::open(kind, label, rows));
        }
    }

    pub(super) fn handle_ref_picker(&mut self, key: Key) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let Some(picker) = form.picker.as_mut() else {
            return;
        };
        match picker.key(key) {
            form::Picked::Filtered | form::Picked::Moved | form::Picked::Ignored => {}
            form::Picked::Cancelled => {
                form.picker = None;
                form.loading = false;
            }
            // The extId, not the name: the body carries the reference, and the name is only
            // what made the row findable.
            form::Picked::Chose(ext_id) => {
                form.picker = None;
                form.set_selected_value(ext_id);
            }
        }
    }

    pub(super) fn form_error(&mut self, message: String) {
        if let Some(form) = self.form.as_mut() {
            form.error = Some(message);
        }
    }

    /// `enter` on the form: the strings it collected are kept on the pending action and the
    /// confirm follows. `build_body`'s refusal stays under the form with nothing sent.
    ///
    /// The body itself is built in `run_pending`, once per row. Only the refusals the user can
    /// still do something about - a required field left empty, a malformed JSON - are worth
    /// finding here, and a body built now would be one row's, cloned onto every other.
    pub(super) fn submit_form(&mut self) {
        // Owned or `Copy` first, so nothing borrows `self` while the result is applied.
        let (Some(action), Some(entered)) = (
            self.pending.as_ref().map(|p| p.action),
            self.form.as_ref().map(Form::entered),
        ) else {
            return;
        };
        let now = self.now;
        // Checked against the first of the pending rows still in the store, which is what the
        // confirm dialog names. `None` is no session, or every row gone while the form sat
        // open, and either way there is nothing to substitute against.
        let checked = self.live.as_ref().and_then(|live| {
            let view = live.table()?;
            let table = live.store.table(&view.key);
            let pending = self.pending.as_ref()?;
            let entity = pending.rows.iter().find_map(|(id, _)| table.rows.get(id))?;
            Some(nutsh_core::actions::build_body(
                action.form,
                &entered,
                entity,
                now,
            ))
        });
        match checked {
            // Values are never journalled; the strings go straight onto the pending action.
            Some(Ok(_)) => {
                if let Some(p) = self.pending.as_mut() {
                    p.entered = Some(entered);
                }
                self.form = None;
                self.confirm_or_run();
            }
            Some(Err(e)) => self.form_error(e),
            None => self.form_error("the row this action was started on is gone".into()),
        }
    }

    /// The add form and the login field, likewise.
    pub(super) fn handle_form(&mut self, key: Key) {
        let action = self.screen.form_key(key);
        self.screen_action(action);
    }
}
