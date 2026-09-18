//! The one-row probe: whether a table changed since the last walk, and what that costs.

use super::*;

/// Consecutive skips before a cycle walks whatever the probe says.
pub(crate) const MAX_SKIPS: u32 = 9;

/// What the probe knows about one table between cycles. **Never derived from `rows`:** the rows
/// are in the table's own `orderby` - creation order for both armed kinds - while `probe_by`
/// is last-modified, so the two name different rows almost always and a comparison against
/// `rows[0]` would mismatch on its first cycle and disarm by construction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ProbeState {
    /// Not calibrated yet: the next cycle probes *and* walks.
    #[default]
    Unarmed,
    /// This Prism Central did not honour `$orderby` on `probe_by`, or reports no total. Never
    /// probe this table again this session.
    Disarmed,
    Armed {
        sort_value: String,
        ext_id: String,
        total: u64,
        skips: u32,
    },
}

/// One `$limit=1` answer: the envelope's total and one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeAnswer {
    pub sort_value: Option<String>,
    /// `None` when the collection was empty. Distinct from a row that carries no `probe_by`
    /// value: an empty table teaches calibration nothing, where a row without the field
    /// proves the probe can never decide.
    pub ext_id: Option<String>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeDecision {
    Skip,
    Walk,
}

/// Skip the walk only when the newest row by last-modified is the same row with the same
/// timestamp *and* the count is unchanged. A stronger claim than "the first row of page 0 is
/// unchanged", for the same one request.
pub(crate) fn probe_decision(
    state: &ProbeState,
    answer: &ProbeAnswer,
    dirty: bool,
) -> ProbeDecision {
    if dirty {
        return ProbeDecision::Walk;
    }
    let ProbeState::Armed {
        sort_value,
        ext_id,
        total,
        skips,
    } = state
    else {
        return ProbeDecision::Walk;
    };
    if *skips >= MAX_SKIPS {
        return ProbeDecision::Walk;
    }
    let (Some(seen), Some(seen_id), Some(now_total)) =
        (&answer.sort_value, &answer.ext_id, answer.total)
    else {
        return ProbeDecision::Walk;
    };
    if seen == sort_value && seen_id == ext_id && now_total == *total {
        ProbeDecision::Skip
    } else {
        ProbeDecision::Walk
    }
}

/// What a cycle that probed and walked leaves behind. A probe is sound only if the Prism
/// Central honours `$orderby` on `probe_by`: `walk` is `Some` on the one cycle that checks -
/// the first eligible, which walks before it probes - and `None` afterwards. Both sides are
/// parsed, not compared as bytes: Prism trims trailing zeros from fractional seconds, and a
/// shorter fraction compares greater by byte order while being earlier in time.
pub(crate) fn calibrate(
    state: &ProbeState,
    answer: &ProbeAnswer,
    walk: Option<&Walk>,
) -> ProbeState {
    if *state == ProbeState::Disarmed {
        return ProbeState::Disarmed;
    }
    if answer.ext_id.is_none() && answer.sort_value.is_none() {
        // The collection was empty when the probe asked. Nothing was proven either way, and a
        // table that merely happens to hold no rows for one cycle must not lose its probe for
        // the session.
        return state.clone();
    }
    let (Some(sort_value), Some(ext_id), Some(total)) = (
        answer.sort_value.clone(),
        answer.ext_id.clone(),
        answer.total,
    ) else {
        // No total to compare, or a row that does not carry the field: nothing the probe could
        // ever decide on.
        return ProbeState::Disarmed;
    };
    let probed = instant_of(&sort_value);
    // The calibrating cycle requires the comparison: an unparseable probe answer, or a walk with
    // no readable `probe_by` value, is a PC not answering the timestamp its spec declares, and
    // arming on it is the one thing this check exists to prevent. An empty walk proves nothing.
    if *state == ProbeState::Unarmed
        && (probed.is_none() || walk.is_some_and(|w| w.max_sort.is_none() && w.saw_rows))
    {
        return ProbeState::Disarmed;
    }
    if let Some(max) = walk.and_then(|w| w.max_sort.as_deref()) {
        match (probed, instant_of(max)) {
            // A value that will not parse is a value the probe cannot compare, whichever side
            // it is on.
            (Some(probed), Some(newest)) if probed >= newest => {}
            _ => return ProbeState::Disarmed,
        }
    }
    ProbeState::Armed {
        sort_value,
        ext_id,
        total,
        skips: 0,
    }
}

/// One RFC 3339 timestamp as an instant, or `None` when the value is not one.
pub(super) fn instant_of(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

/// The probe's state for one subscription, shared between the `Scheduler`'s entry and the task
/// that polls. It lives here rather than on `store::Table` because a `Table` is inside `Store`
/// on the UI thread and this task holds no reference to one: the only reader and the only
/// writer of a probe baseline are both here.
#[derive(Debug, Default)]
pub(crate) struct ProbeCell {
    pub(super) state: Mutex<ProbeState>,
    /// The next cycle must walk, whatever the probe would say.
    pub(super) dirty: AtomicBool,
    /// Pages the last completed walk took. Below two, probing costs what walking costs.
    pub(super) pages: AtomicU32,
}

impl ProbeCell {
    pub(super) fn state(&self) -> ProbeState {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) fn set(&self, state: ProbeState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
    }

    pub(super) fn pages(&self) -> u32 {
        self.pages.load(Ordering::Relaxed)
    }

    pub(super) fn set_pages(&self, pages: u32) {
        self.pages.store(pages, Ordering::Relaxed);
    }

    pub(crate) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub(super) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    /// A skip: the baseline stands, and one more skip is on the clock.
    pub(super) fn skipped(&self) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let ProbeState::Armed { skips, .. } = &mut *guard {
            *skips += 1;
        }
    }
}
