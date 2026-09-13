//! The summary boxes: the one part of a page that is code rather than data, because they
//! compute rather than list.

use nutsh_catalog::Role;
use nutsh_core::store::Store;

/// A right-hand column line: `label` dim, `value` in `theme::role_fg(role)`. A value the
/// sampler or the counters could not determine is `-`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryLine {
    pub label: String,
    pub value: String,
    pub role: Role,
}

impl SummaryLine {
    pub fn new(label: &str, value: String, role: Role) -> SummaryLine {
        SummaryLine {
            label: label.to_string(),
            value,
            role,
        }
    }
}

/// The page whose summary the sampler feeds, spelled once: `App` reads it to decide whether to
/// start the sampler and `build` to decide what to draw, and a rename in `pages.toml` that
/// reached only one of them would silently stop the block being filled.
pub(crate) const DISASTER_RECOVERY: &str = "disaster-recovery";

/// The page whose summary the stats poller feeds, spelled once for the same reason - even
/// though only `build` reads it today, since a page id that matched nothing here would leave
/// the box empty with nothing to say so. `every_declared_summary_is_built` is what says so.
pub(crate) const DASHBOARD: &str = "dashboard";

/// The box for `id`, or nothing when the page declares none.
pub fn build(id: &str, store: &Store) -> Vec<SummaryLine> {
    match id {
        DISASTER_RECOVERY => disaster_recovery(store),
        DASHBOARD => dashboard(store),
        _ => Vec::new(),
    }
}

/// Four `$limit=1` counters and the bounded sampler's tallies.
///
/// The last three lines are `-` when the endpoint is not served at all - `Sampled VMs` above
/// them always prints its count, which is `0` until the first cycle lands - and the block
/// labels itself `Sampled VMs N` so the number never claims to be a census.
fn disaster_recovery(store: &Store) -> Vec<SummaryLine> {
    use nutsh_core::stats::show;
    let s = store.sample();
    let count = |label: &str, c| SummaryLine::new(label, show(c), Role::Neutral);
    let tally = |label: &str, n: u64, role| {
        SummaryLine::new(
            label,
            if s.available {
                n.to_string()
            } else {
                "-".to_string()
            },
            if s.available { role } else { Role::Muted },
        )
    };
    vec![
        count("Policies", s.policies),
        count("Recovery plans", s.plans),
        count("Recovery points", s.recovery_points),
        count("Recovery jobs", s.jobs),
        SummaryLine::new("Sampled VMs", s.sampled.to_string(), Role::Neutral),
        tally("  in sync", s.in_sync, Role::Ok),
        tally("  syncing", s.syncing, Role::Pending),
        tally(
            "  out of sync",
            s.out_of_sync,
            if s.out_of_sync > 0 {
                Role::Error
            } else {
                Role::Muted
            },
        ),
    ]
}

/// The header's counters with their totals spelled out, which is what the header has no room
/// for. `-` wherever the poller could not answer.
fn dashboard(store: &Store) -> Vec<SummaryLine> {
    use nutsh_core::stats::show;
    let s = store.stats();
    let line = |label: &str, c, role| SummaryLine::new(label, show(c), role);
    vec![
        line("Clusters", s.clusters, Role::Neutral),
        line("Hosts", s.hosts, Role::Neutral),
        line("Virtual machines", s.vms, Role::Neutral),
        line("  powered on", s.vms_on, Role::Ok),
        line("  powered off", s.vms_off(), Role::Off),
        line(
            "Critical alerts",
            s.alerts_critical,
            if s.alerts_critical.unwrap_or(0) > 0 {
                Role::Error
            } else {
                Role::Muted
            },
        ),
        line(
            "Warning alerts",
            s.alerts_warning,
            if s.alerts_warning.unwrap_or(0) > 0 {
                Role::Warn
            } else {
                Role::Muted
            },
        ),
        line(
            "Running tasks",
            s.tasks_running,
            if s.tasks_running.unwrap_or(0) > 0 {
                Role::Pending
            } else {
                Role::Muted
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `summary = "..."` in `pages.toml` names a box this module builds. Nothing else
    /// checks the two spellings against each other: a renamed page would draw an empty column
    /// where its numbers belong, and the frame would look like a page whose sampler had not
    /// reported yet.
    #[test]
    fn every_declared_summary_is_built() {
        let store = Store::default();
        let mut declared = 0;
        for page in nutsh_catalog::PAGES {
            let Some(id) = page.summary else { continue };
            declared += 1;
            assert!(
                !build(id, &store).is_empty(),
                "{}: no summary box is built for {id:?}",
                page.id
            );
        }
        assert_eq!(declared, 2, "the pages that declare a summary");
    }

    /// The roles the Dashboard's block assigns, which no text snapshot can see: the three
    /// conditional lines warn only when there is something to warn about, and every line is a
    /// muted `-` before the first cycle lands.
    #[test]
    fn the_dashboard_lines_carry_the_roles_the_counters_are_for() {
        use nutsh_core::stats::Stats;

        let built = |stats| {
            let mut store = Store::default();
            store.set_stats(stats);
            build(DASHBOARD, &store)
                .into_iter()
                .map(|l| (l.label, l.value, l.role))
                .collect::<Vec<_>>()
        };
        let line = |label: &str, value: &str, role| (label.to_string(), value.to_string(), role);

        assert_eq!(
            built(Stats {
                clusters: Some(2),
                hosts: Some(8),
                vms: Some(31),
                vms_on: Some(29),
                alerts_critical: Some(1),
                alerts_warning: Some(4),
                tasks_running: Some(2),
                stale: false,
            }),
            vec![
                line("Clusters", "2", Role::Neutral),
                line("Hosts", "8", Role::Neutral),
                line("Virtual machines", "31", Role::Neutral),
                line("  powered on", "29", Role::Ok),
                line("  powered off", "2", Role::Off),
                line("Critical alerts", "1", Role::Error),
                line("Warning alerts", "4", Role::Warn),
                line("Running tasks", "2", Role::Pending),
            ]
        );
        // Nothing wrong is not a warning, and nothing running is not pending.
        assert_eq!(
            built(Stats {
                alerts_critical: Some(0),
                alerts_warning: Some(0),
                tasks_running: Some(0),
                ..Default::default()
            })[5..],
            [
                line("Critical alerts", "0", Role::Muted),
                line("Warning alerts", "0", Role::Muted),
                line("Running tasks", "0", Role::Muted),
            ]
        );
        // Before the first cycle, and wherever the counter could not be read: every value is a
        // dash, and the three conditional lines are muted rather than alarming.
        assert_eq!(
            built(Stats::default()),
            vec![
                line("Clusters", "-", Role::Neutral),
                line("Hosts", "-", Role::Neutral),
                line("Virtual machines", "-", Role::Neutral),
                line("  powered on", "-", Role::Ok),
                line("  powered off", "-", Role::Off),
                line("Critical alerts", "-", Role::Muted),
                line("Warning alerts", "-", Role::Muted),
                line("Running tasks", "-", Role::Muted),
            ]
        );
    }
}
