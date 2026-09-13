//! Watching the Prism Central task an action returned. Task entities land in the ordinary
//! Tasks table, so `:tasks` and the watcher never disagree and the watcher needs no store
//! machinery of its own.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use nutsh_catalog::Kind;
use nutsh_prism::{Entity, TaskRef};
use serde_json::Value;

use crate::actions::Plan;
use crate::journal::{JournalId, JournalOutcome};
use crate::scheduler::{Scheduler, SubId, Subscription};
use crate::store::TableKey;

/// A watch still running after this long gives up. Never reported as a failure - the task may
/// still succeed, and it stays in the Tasks table either way.
const GIVE_UP_AFTER: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    Canceling,
    Succeeded,
    Failed,
    Canceled,
    Suspended,
    /// `$UNKNOWN`, `$REDACTED`, an absent `status`, or a member the spec gains later. The v4.4
    /// `TaskStatus` has exactly nine, and folding the last two into `Queued` would report
    /// `queued 0%` for a field this account may not read - then poll it for fifteen minutes.
    Unknown,
}

impl TaskStatus {
    pub fn parse(s: &str) -> TaskStatus {
        match s {
            "QUEUED" => TaskStatus::Queued,
            "RUNNING" => TaskStatus::Running,
            "CANCELING" | "CANCELLING" => TaskStatus::Canceling,
            "SUCCEEDED" => TaskStatus::Succeeded,
            "FAILED" => TaskStatus::Failed,
            "CANCELED" | "CANCELLED" => TaskStatus::Canceled,
            "SUSPENDED" => TaskStatus::Suspended,
            _ => TaskStatus::Unknown,
        }
    }

    /// `SUSPENDED` is **not** terminal - a suspended task can resume - but the status line
    /// reports it as one.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Canceled
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::Canceling => "cancelling",
            TaskStatus::Succeeded => "succeeded",
            TaskStatus::Failed => "failed",
            TaskStatus::Canceled => "cancelled",
            TaskStatus::Suspended => "suspended",
            TaskStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskWatch {
    pub task: String,
    pub kind: &'static Kind,
    /// The row's kind, which is what a refresh has to be issued against.
    pub row_kind: &'static Kind,
    pub ext_id: String,
    pub name: String,
    pub action: &'static str,
    pub parents: Vec<String>,
    pub status: TaskStatus,
    pub progress: u8,
    pub started: Instant,
    pub sub: SubId,
    pub errors: Vec<String>,
    pub completion: Vec<(String, String)>,
    pub cancelable: bool,
    pub journal: JournalId,
}

impl TaskWatch {
    /// What every watch starts at, whatever supplied the identifiers. Both constructors spread
    /// `..` over this, so a new field is written once.
    fn fresh(started: Instant, sub: SubId, journal: JournalId) -> TaskWatch {
        TaskWatch {
            task: String::new(),
            kind: task_kind(),
            row_kind: task_kind(),
            ext_id: String::new(),
            name: String::new(),
            action: "",
            parents: Vec::new(),
            status: TaskStatus::Queued,
            progress: 0,
            started,
            sub,
            errors: Vec::new(),
            completion: Vec::new(),
            cancelable: true,
            journal,
        }
    }

    /// `power-off web-01 running 50%`.
    pub fn line(&self) -> String {
        match self.status {
            TaskStatus::Running | TaskStatus::Queued => format!(
                "{} {} {} {}%",
                self.action,
                self.name,
                self.status.label(),
                self.progress
            ),
            other => format!("{} {} {}", self.action, self.name, other.label()),
        }
    }
}

/// A watch that will not be polled again.
#[derive(Debug, Clone)]
pub struct Finished {
    pub watch: TaskWatch,
    pub outcome: JournalOutcome,
}

impl Finished {
    pub fn outcome_text(&self) -> String {
        self.outcome.label().into_owned()
    }
}

#[derive(Debug, Default)]
pub struct TaskIndex {
    /// Entity extId → the tasks that touched it, recorded for watched tasks only. The DR page
    /// reads this instead of opening a Task subscription of its own, so `TaskIndex` is a
    /// read-only public surface of `core`.
    ///
    /// Never pruned, so what may enter it is what bounds it: one entry per entity a task this
    /// session started touched. Recording every task entity the Tasks table drains would grow
    /// a `String` key and a `Vec<String>` per row, for the whole life of the session.
    by_entity: HashMap<String, Vec<String>>,
    watched: IndexMap<String, TaskWatch>,
}

impl TaskIndex {
    /// Follow `task` with a single subscription over the Task kind; its `poll_secs` is 3.
    pub fn watch(
        &mut self,
        scheduler: &mut Scheduler,
        plan: &Plan,
        task: TaskRef,
        journal: JournalId,
    ) -> SubId {
        let key = TableKey::top(task_kind());
        let interval = Duration::from_secs(u64::from(key.kind.poll_secs.max(1)));
        // `watching`: the idle pause and the refresh schedule both leave a watch alone. It is
        // the user's own pending mutation, it already expires after fifteen minutes, and
        // pausing it would strand exactly the thing somebody is waiting on.
        let sub = scheduler
            .subscribe(Subscription::single(key, interval, task.ext_id.clone()).watching());
        self.watched.insert(
            task.ext_id.clone(),
            TaskWatch {
                task: task.ext_id,
                kind: plan.kind,
                row_kind: plan.row_kind,
                ext_id: plan.ext_id.clone(),
                name: plan.name.clone(),
                action: plan.action.name,
                parents: plan.parents.clone(),
                ..TaskWatch::fresh(Instant::now(), sub, journal)
            },
        );
        sub
    }

    /// A task entity from the poll channel. Returns the watch when it will not be polled again,
    /// having removed it; the caller unsubscribes, settles the journal entry, and refreshes.
    pub fn apply(&mut self, entity: &Entity) -> Option<Finished> {
        let id = entity.ext_id.as_str();
        let raw = &entity.raw;
        // Nothing is recorded for a task nobody asked about: the Tasks table drains every task
        // on the Prism Central through here, and `by_entity` is never pruned.
        if !self.watched.contains_key(id) {
            return None;
        }
        // `entitiesAffected` lists what the task acted on, so the created entity is the one
        // that is *not* the source.
        for affected in raw
            .get("entitiesAffected")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| e.get("extId").and_then(Value::as_str))
        {
            let tasks = self.by_entity.entry(affected.to_string()).or_default();
            if !tasks.iter().any(|t| t == id) {
                tasks.push(id.to_string());
            }
        }
        let watch = self.watched.get_mut(id)?;
        watch.status = TaskStatus::parse(raw.get("status").and_then(Value::as_str).unwrap_or(""));
        watch.progress = raw
            .get("progressPercentage")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(100) as u8;
        watch.cancelable = raw
            .get("isCancelable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        watch.errors = strings(raw, "errorMessages", "message");
        watch.completion = raw
            .get("completionDetails")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|d| {
                        Some((
                            d.get("name")?.as_str()?.to_string(),
                            d.get("value")?.as_str().unwrap_or("").to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !watch.status.is_terminal() {
            return None;
        }
        let outcome = match watch.status {
            TaskStatus::Succeeded => JournalOutcome::Succeeded,
            TaskStatus::Failed => JournalOutcome::Failed(if watch.errors.is_empty() {
                "the task failed".to_string()
            } else {
                watch.errors.join("; ")
            }),
            // Named, not `_`: `is_terminal` admits exactly these three today, and a fourth
            // added there must be worded rather than silently reported as a cancellation.
            TaskStatus::Canceled => JournalOutcome::Cancelled,
            other => JournalOutcome::Unknown(other.label().to_string()),
        };
        let watch = self.watched.shift_remove(id)?;
        Some(Finished { watch, outcome })
    }

    /// Watches past the 15-minute cap, removed. The task stays in the Tasks table.
    pub fn expire(&mut self, now: Instant) -> Vec<Finished> {
        let stale: Vec<String> = self
            .watched
            .iter()
            .filter(|(_, w)| now.duration_since(w.started) >= GIVE_UP_AFTER)
            .map(|(id, _)| id.clone())
            .collect();
        stale
            .into_iter()
            .filter_map(|id| {
                let watch = self.watched.shift_remove(&id)?;
                Some(Finished {
                    watch,
                    outcome: JournalOutcome::Unknown("still running after 15m".to_string()),
                })
            })
            .collect()
    }

    /// The watches still being polled, in the order they were opened. An iterator, not a
    /// `Vec`: the status line asks per frame, and the draw loop does not allocate.
    pub fn running(&self) -> impl Iterator<Item = &TaskWatch> {
        self.watched.values()
    }

    pub fn status_of(&self, task: &str) -> Option<TaskStatus> {
        self.watched.get(task).map(|w| w.status)
    }

    /// The tasks that have touched `ext_id`, newest last.
    pub fn for_entity(&self, ext_id: &str) -> &[String] {
        self.by_entity.get(ext_id).map_or(&[], Vec::as_slice)
    }

    /// Tests only: a watch with no subscription behind it, to exercise the 15-minute cap.
    /// `pub` because an integration test cannot see `#[cfg(test)]`; hidden because it is not
    /// part of what this type offers.
    #[doc(hidden)]
    pub fn insert_for_test(&mut self, task: &str, started: Instant) {
        self.watched.insert(
            task.to_string(),
            TaskWatch {
                task: task.to_string(),
                name: "t".into(),
                action: "cancel",
                status: TaskStatus::Running,
                ..TaskWatch::fresh(started, SubId(0), JournalId::default())
            },
        );
    }
}

fn task_kind() -> &'static Kind {
    nutsh_catalog::kind("prism.config.Task").expect("the catalog has tasks")
}

fn strings(raw: &Value, array: &str, field: &str) -> Vec<String> {
    raw.get(array)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get(field).and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
