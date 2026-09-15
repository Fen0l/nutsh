//! The journal screen.

use super::*;

impl App {
    /// `:journal`. The entries live in `Live`, so without a session there is nothing to list.
    pub(super) fn journal_command(&mut self) {
        if self.live.is_none() {
            self.status = Some("not connected".into());
            return;
        }
        self.journal_view = Some(JournalView::default());
        self.journal_from = self.mode;
        self.mode = Mode::Journal;
    }

    pub(super) fn handle_journal(&mut self, key: Key) {
        let last = self.journal_len().saturating_sub(1);
        let Some(view) = self.journal_view.as_mut() else {
            return;
        };
        match view.key(key, last) {
            journal::Action::Closed => {
                self.journal_view = None;
                self.mode = self.journal_from;
            }
            journal::Action::Moved | journal::Action::Ignored => {}
        }
    }

    /// How many entries `:journal` is listing; the cursor is measured off it.
    pub(super) fn journal_len(&self) -> usize {
        self.live.as_ref().map_or(0, |l| l.journal.entries().len())
    }

    pub(super) fn journal_refusal(
        &mut self,
        action: &'static Action,
        subjects: &[(String, String)],
        reason: &str,
    ) {
        let now = self.now;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(view) = live.table() else { return };
        let kind = view.key.kind;
        let context = live
            .session
            .context
            .clone()
            .unwrap_or_else(|| "(env)".into());
        for (ext_id, name) in subjects {
            live.journal.record(
                Attempt {
                    context: &context,
                    kind,
                    ext_id,
                    name,
                    action: action.name,
                },
                now,
                JournalOutcome::Denied(reason.to_string()),
            );
        }
    }
}
