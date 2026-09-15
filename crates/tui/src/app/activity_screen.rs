//! The activity screen.

use super::*;

impl App {
    /// `:activity`. The ring lives in the client and the tables in the store, so without a
    /// session there is nothing to list.
    pub(super) fn activity_command(&mut self) {
        if self.live.is_none() {
            self.status = Some("not connected".into());
            return;
        }
        self.activity_view = Some(crate::activity::ActivityView::default());
        self.activity_from = self.mode;
        self.mode = Mode::Activity;
    }

    pub(super) fn handle_activity(&mut self, key: Key) {
        let last = self.activity_len().saturating_sub(1);
        let Some(view) = self.activity_view.as_mut() else {
            return;
        };
        match view.key(key, last) {
            crate::activity::Action::Closed => {
                self.activity_view = None;
                self.mode = self.activity_from;
            }
            crate::activity::Action::Moved | crate::activity::Action::Ignored => {}
        }
        self.dirty = true;
    }

    /// How many rows the tab showing has; the cursor is measured off it.
    pub(super) fn activity_len(&self) -> usize {
        let (Some(live), Some(view)) = (self.live.as_ref(), self.activity_view.as_ref()) else {
            return 0;
        };
        match view.tab {
            crate::activity::Tab::Requests => live.session.client.metrics().calls().len(),
            crate::activity::Tab::Tables => live.store.tables().count(),
        }
    }

    /// Every table the session holds, most recently polled first, never polled last. What the
    /// `TABLES` tab draws; a `Vec` because it is sorted, and the screen is drawn once a frame.
    pub(crate) fn activity_tables(&self) -> Vec<TableLine> {
        let Some(live) = self.live.as_ref() else {
            return Vec::new();
        };
        let now = std::time::Instant::now();
        let mut out: Vec<TableLine> = live
            .store
            .tables()
            .map(|(key, t)| TableLine {
                kind: key.kind,
                scope: match (&key.parents[..], key.filter.as_deref()) {
                    ([], None) => String::new(),
                    (parents, None) => format!("under {}", parents.join("/")),
                    (_, Some(f)) => f.to_string(),
                },
                rows: t.rows.len(),
                total: t.total,
                age: t
                    .last_poll
                    .map(|at| now.saturating_duration_since(at).as_secs()),
                from: if t.restored_at.is_some() {
                    "cache"
                } else if t.last_poll.is_some() {
                    "wire"
                } else {
                    "-"
                },
                // `not served` before the error text: a 404 sets both, and the word is the
                // one the table's own title uses.
                state: match (&t.error, t.not_served, t.loading) {
                    (_, true, _) => "not served".to_string(),
                    (Some(e), _, _) => e.upstream.clone(),
                    (None, false, true) => "loading".to_string(),
                    _ => "ok".to_string(),
                },
            })
            .collect();
        out.sort_by_key(|l| (l.age.is_none(), l.age.unwrap_or(0), l.kind.display));
        out
    }
}

/// One row of the `TABLES` tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLine {
    pub kind: &'static Kind,
    /// The parents or the filter that narrow it; empty for a top-level table listed whole.
    pub scope: String,
    pub rows: usize,
    pub total: Option<u64>,
    /// Seconds since the last completed cycle; `None` for a table that has never polled.
    pub age: Option<u64>,
    pub from: &'static str,
    pub state: String,
}
