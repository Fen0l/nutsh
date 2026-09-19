//! What was attempted this session: identifiers and outcomes, never a request body, never a
//! password, never a form value.
//!
//! Not persisted, by design: a file of what an administrator did is an audit log, and an
//! editable audit log under `~/.local/state` is worse than none. Prism Central's own audit
//! trail is the record of truth; this is a session's scrollback.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::collections::vec_deque;
use std::time::SystemTime;

use nutsh_catalog::Kind;

/// Stable across the ring's eviction: settling an id that has been dropped is a no-op.
///
/// `Default` is the reserved id zero, which no `Journal` ever issues - `next` starts at one -
/// so a `TaskWatch` built outside one (a test's, or a watch whose attempt was never journalled)
/// carries an id that matches no entry and settling it changes nothing. Handing out zero would
/// make that watch alias the session's first real attempt and overwrite its outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JournalId(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalOutcome {
    Started,
    Succeeded,
    Failed(String),
    Denied(String),
    /// The task was cancelled, by this session or another. Its own variant rather than an
    /// `Unknown`: `unknown: cancelled` contradicts itself on the one line that is supposed to
    /// say what happened.
    Cancelled,
    /// No outcome was ever observed - the watch gave up, or a status arrived that the catalog
    /// does not model. The string says which; the label prefixes it with `unknown:`.
    Unknown(String),
}

impl JournalOutcome {
    /// One row's worth of words. Borrowed wherever the text already exists: this runs once per
    /// visible row per frame.
    pub fn label(&self) -> Cow<'_, str> {
        match self {
            JournalOutcome::Started => Cow::Borrowed("started"),
            JournalOutcome::Succeeded => Cow::Borrowed("succeeded"),
            JournalOutcome::Failed(m) => Cow::Owned(format!("failed: {m}")),
            // Not `denied: {m}`: `core::actions::reason` already worded it, and prefixing a
            // second time prints `denied: denied: …`.
            JournalOutcome::Denied(m) => Cow::Borrowed(m),
            JournalOutcome::Cancelled => Cow::Borrowed("cancelled"),
            JournalOutcome::Unknown(m) => Cow::Owned(format!("unknown: {m}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: JournalId,
    pub at: SystemTime,
    pub context: String,
    /// The kind itself, not its id: the catalog's kinds are `'static`, the row is drawn once a
    /// frame, and a view that has the `Kind` in hand never has to look one up to name it.
    pub kind: &'static Kind,
    pub ext_id: String,
    pub name: String,
    pub action: &'static str,
    pub outcome: JournalOutcome,
    pub task_ext_id: Option<String>,
}

/// The identifiers of one attempt, named at the call site: `ext_id` and `name` are two
/// adjacent strings, and a positional call that swapped them would compile and only be caught
/// by eye in the journal view.
#[derive(Debug, Clone, Copy)]
pub struct Attempt<'a> {
    pub context: &'a str,
    pub kind: &'static Kind,
    pub ext_id: &'a str,
    pub name: &'a str,
    pub action: &'static str,
}

const CAPACITY: usize = 500;

#[derive(Debug)]
pub struct Journal {
    entries: VecDeque<Entry>,
    next: u64,
}

impl Default for Journal {
    /// `next` starts at one, not zero: `JournalId::default()` is the id a watch carries when
    /// its attempt was never journalled, and it must match no entry.
    fn default() -> Journal {
        Journal {
            entries: VecDeque::new(),
            next: 1,
        }
    }
}

impl Journal {
    /// Append at attempt time. Every path that could mutate writes one, including those that
    /// never reach the network: a `Deny` is recorded, so `:journal` answers "why did nothing
    /// happen" as well as "what did I do".
    pub fn record(
        &mut self,
        attempt: Attempt<'_>,
        at: SystemTime,
        outcome: JournalOutcome,
    ) -> JournalId {
        let id = JournalId(self.next);
        self.next += 1;
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry {
            id,
            at,
            context: attempt.context.to_string(),
            kind: attempt.kind,
            ext_id: attempt.ext_id.to_string(),
            name: attempt.name.to_string(),
            action: attempt.action,
            outcome,
            task_ext_id: None,
        });
        id
    }

    /// Update in place when the watch finishes, so one row tells the whole story.
    pub fn settle(&mut self, id: JournalId, outcome: JournalOutcome, task: Option<String>) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.outcome = outcome;
            if task.is_some() {
                e.task_ext_id = task;
            }
        }
    }

    /// Oldest first; the view reverses, and windows with `len`, `rev`, `skip` and `take`.
    /// The context the entry `id` was recorded against.
    pub fn context_of(&self, id: JournalId) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.context.as_str())
    }

    pub fn entries(&self) -> vec_deque::Iter<'_, Entry> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
