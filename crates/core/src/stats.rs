//! The session's counters: what the header's stats block shows, and the poller that fills
//! them.
//!
//! Every counter is `None` until the first cycle lands, and `None` renders `-`, which is also
//! what a counter this Prism Central cannot answer renders.

use std::sync::Arc;
use std::time::Duration;

use nutsh_prism::{Client, ListOptions, PrismError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::scheduler::Msg;

/// Between cycles. Seven small requests every thirty seconds is nothing; a burst of seven is
/// exactly what a rate limiter notices, so they go out one at a time.
const PERIOD: Duration = Duration::from_secs(30);

/// A counter, or `-` when it could not be read.
pub type Count = Option<u64>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub clusters: Count,
    pub hosts: Count,
    pub vms: Count,
    pub vms_on: Count,
    pub alerts_critical: Count,
    pub alerts_warning: Count,
    pub tasks_running: Count,
    /// Every request of the last cycle failed; the values are the previous ones and the block
    /// is drawn dim. Showing yesterday's counts as though they were live is worse than showing
    /// them greyed, and blanking a screenful of numbers because one request timed out is worse
    /// than both. A cycle that issued no request at all - this Prism Central serves none of
    /// these namespaces - is not stale: it has no numbers to go stale, and says so with `-`.
    pub stale: bool,
}

impl Stats {
    /// `vms - vms_on`, and only when both are known.
    pub fn vms_off(&self) -> Count {
        Some(self.vms?.saturating_sub(self.vms_on?))
    }
}

/// A counter as the block prints it.
pub fn show(count: Count) -> String {
    match count {
        Some(n) => n.to_string(),
        None => "-".to_string(),
    }
}

/// The seven counters, with the `$filter` each needs. The filters are constants with their
/// spec citation, not generated: Nutanix v4's enum-literal prefix (`Vmm.Ahv.Config.PowerState`)
/// is not derivable from the kind id (`vmm.ahv.config.Vm`), and pretending otherwise would be
/// a helper that is wrong for the first kind nobody checked.
struct CounterDef {
    counter: Counter,
    kind: &'static str,
    filter: Option<&'static str>,
    /// The property a pinned cluster is matched on, or `None` for a counter that cannot be
    /// scoped - which then reads `-` rather than a fleet-wide number presented as the pinned
    /// cluster's. Only the two properties a recorded lab response actually carries are here;
    /// `clustermgmt.config.Host` nests its cluster reference under `cluster.uuid` and
    /// `prism.config.Task` carries an *array* (`clusterExtIds`), and neither has a filter
    /// spelling this project has seen a Prism Central accept. Either becomes `None` if it comes
    /// back 400.
    cluster_on: Option<&'static str>,
}

const COUNTERS: &[CounterDef] = &[
    CounterDef {
        counter: Counter::Clusters,
        kind: "clustermgmt.config.Cluster",
        filter: None,
        cluster_on: None,
    },
    CounterDef {
        counter: Counter::Hosts,
        kind: "clustermgmt.config.Host",
        filter: None,
        cluster_on: None,
    },
    CounterDef {
        counter: Counter::Vms,
        kind: "vmm.ahv.config.Vm",
        filter: None,
        cluster_on: Some("cluster/extId"),
    },
    CounterDef {
        counter: Counter::VmsOn,
        kind: "vmm.ahv.config.Vm",
        filter: Some("powerState eq Vmm.Ahv.Config.PowerState'ON'"),
        cluster_on: Some("cluster/extId"),
    },
    CounterDef {
        counter: Counter::AlertsCritical,
        kind: "monitoring.serviceability.Alert",
        filter: Some("severity eq Monitoring.Common.Severity'CRITICAL' and isResolved eq false"),
        cluster_on: Some("clusterUUID"),
    },
    CounterDef {
        counter: Counter::AlertsWarning,
        kind: "monitoring.serviceability.Alert",
        filter: Some("severity eq Monitoring.Common.Severity'WARNING' and isResolved eq false"),
        cluster_on: Some("clusterUUID"),
    },
    // The same filter spells the Dashboard's "Running tasks" pane (`crates/catalog/pages.toml`),
    // so that the header's number and the pane's rows count one thing;
    // `the_running_tasks_counter_and_the_dashboard_pane_share_one_filter` fails if one is
    // corrected without the other.
    CounterDef {
        counter: Counter::TasksRunning,
        kind: "prism.config.Task",
        filter: Some("status eq Prism.Config.TaskStatus'RUNNING'"),
        cluster_on: None,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Counter {
    Clusters,
    Hosts,
    Vms,
    VmsOn,
    AlertsCritical,
    AlertsWarning,
    TasksRunning,
}

/// One cycle every thirty seconds until the handle is aborted.
///
/// `cluster` is the session's pin: every counter that can be scoped is scoped, and every
/// counter that cannot be is `-` rather than a fleet-wide number presented as the pinned
/// cluster's.
pub fn spawn(
    client: Arc<Client>,
    tx: mpsc::Sender<Msg>,
    generation: u64,
    cluster: Option<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut previous = Stats::default();
        // A counter the server refused - a 4xx: this Prism Central does not accept the filter,
        // or this account may not read the kind - is asked once and then dropped, rather than
        // being refused afresh every thirty seconds for ever. Only a refusal is recorded here:
        // a timeout or a 5xx is the service, not the request, and is tried again next cycle.
        let mut refused: Vec<Counter> = Vec::new();
        loop {
            let (stats, outcome) =
                cycle(&client, cluster.as_deref(), &previous, &mut refused).await;
            previous = fold(previous, stats, outcome);
            if tx
                .send(Msg::Stats {
                    generation,
                    stats: previous,
                })
                .await
                .is_err()
            {
                return;
            }
            // A credential this Prism Central refused ends the counters outright, in the cycle
            // that learns it. Seven requests every thirty seconds is how an account gets locked
            // by a program nobody is looking at, and the header's numbers are not worth one
            // attempt at it.
            if client.auth_rejected() {
                return;
            }
            tokio::time::sleep(PERIOD).await;
        }
    })
}

/// What a cycle managed, which is what tells a Prism Central that stopped answering from one
/// that has nothing to answer with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// At least one request came back.
    Answered,
    /// Every request that went out failed.
    AllFailed,
    /// No request went out: none of these counters has anything behind it here.
    NothingAsked,
}

/// The cycle's numbers folded onto the previous ones.
///
/// A cycle where everything failed keeps what was on screen and dims it; a cycle that asked
/// nothing is not a failure - there are no numbers to go stale - so its dashes are drawn as
/// dashes rather than as greyed numbers that were never there.
fn fold(previous: Stats, stats: Stats, outcome: Outcome) -> Stats {
    match outcome {
        Outcome::Answered | Outcome::NothingAsked => stats,
        Outcome::AllFailed => Stats {
            stale: true,
            ..previous
        },
    }
}

/// One pass over the counters, and what it managed.
///
/// The pass starts from `previous` so that a counter whose request failed transiently keeps
/// the number it had rather than flapping to `-` and back; every other outcome - refused,
/// unserved, unfilterable, out of scope - writes its own slot, so no value survives a cycle
/// by accident.
async fn cycle(
    client: &Client,
    cluster: Option<&str>,
    previous: &Stats,
    refused: &mut Vec<Counter>,
) -> (Stats, Outcome) {
    let mut stats = Stats {
        stale: false,
        ..*previous
    };
    let mut asked = false;
    let mut answered = false;
    for def in COUNTERS {
        let counter = def.counter;
        if refused.contains(&counter) {
            *slot(&mut stats, counter) = None;
            continue;
        }
        // `every_counter_names_a_kind_the_catalog_has_and_can_filter` proves every id resolves;
        // a miss would be a regenerated catalog that dropped the kind, leaving the counter
        // with nothing to ask for.
        let Some(kind) = nutsh_catalog::kind(def.kind) else {
            *slot(&mut stats, counter) = None;
            continue;
        };
        // Fixed at connect, so this is an in-memory check rather than a refusal to cache: the
        // namespace is simply not there, this cycle and every other.
        if !client.is_served(kind) {
            *slot(&mut stats, counter) = None;
            continue;
        }
        // A pinned cluster scopes what can be scoped and blanks what cannot: a fleet-wide
        // number presented as the pinned cluster's would be a lie, and `-` is not.
        let scope = match (cluster, def.cluster_on) {
            (None, _) => None,
            (Some(c), Some(field)) => Some(format!("{field} eq '{}'", odata_literal(c))),
            (Some(_), None) => {
                *slot(&mut stats, counter) = None;
                continue;
            }
        };
        let filter = match (def.filter, scope) {
            (Some(f), Some(s)) => Some(format!("{f} and {s}")),
            (Some(f), None) => Some(f.to_string()),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        };
        // A counter that needs a filter on a kind that does not take one has no number to
        // give: the client would drop the `$filter` and the answer would count the fleet.
        if filter.is_some() && !kind.list_params.filter {
            *slot(&mut stats, counter) = None;
            continue;
        }
        let opts = ListOptions {
            limit: 1,
            filter,
            ..Default::default()
        };
        asked = true;
        match client.list_page(kind, 0, &opts).await {
            Ok(page) => {
                answered = true;
                *slot(&mut stats, counter) = page.total;
            }
            // A refusal is the same answer every thirty seconds, so the counter blanks and
            // stops asking. Anything else is the service or the network, and the number the
            // counter last had beats a dash that would flap back on the next cycle.
            Err(e) if is_refusal(&e) => {
                refused.push(counter);
                *slot(&mut stats, counter) = None;
            }
            Err(_) => {}
        }
    }
    if cluster.is_some() {
        // One by definition.
        stats.clusters = Some(1);
    }
    let outcome = match (answered, asked) {
        (true, _) => Outcome::Answered,
        (false, true) => Outcome::AllFailed,
        (false, false) => Outcome::NothingAsked,
    };
    (stats, outcome)
}

/// Whether the server refused the request rather than failed to answer it. A refusal is the
/// same on every cycle - the filter is not a spelling this Prism Central accepts, the path is
/// not there, the account may not read the kind, the credential is wrong - so the counter is
/// dropped instead of being refused for ever.
///
/// `Auth` is not exempt. A rejected credential is the session's problem as well as this
/// counter's, and exempting it here would put one password per subscription on the wire every
/// cycle for as long as the program ran. The session's half is `Client::auth_rejected`, which
/// ends the loop; this is the counter's half, which stops it asking in the cycle where it
/// learns.
///
/// `RateLimited` stays exempt, because it says in so many words to come back.
fn is_refusal(e: &PrismError) -> bool {
    matches!(
        e,
        PrismError::Auth
            | PrismError::Denied(_)
            | PrismError::Forbidden(_)
            | PrismError::NotFound(_)
            | PrismError::Api {
                status: 400..=499,
                ..
            }
    )
}

/// A string as the body of an OData literal: the quote that would end it is doubled, which is
/// how OData escapes one. Prism Central's extIds are server-generated UUIDs, so this is
/// insurance rather than a fix - but an unescaped quote composes a malformed `$filter`, and a
/// malformed filter comes back 400, which this counter would then read as a refusal and drop.
fn odata_literal(s: &str) -> String {
    s.replace('\'', "''")
}

/// Which field a counter writes. One place, so the table above and the struct cannot drift.
fn slot(stats: &mut Stats, counter: Counter) -> &mut Count {
    match counter {
        Counter::Clusters => &mut stats.clusters,
        Counter::Hosts => &mut stats.hosts,
        Counter::Vms => &mut stats.vms,
        Counter::VmsOn => &mut stats.vms_on,
        Counter::AlertsCritical => &mut stats.alerts_critical,
        Counter::AlertsWarning => &mut stats.alerts_warning,
        Counter::TasksRunning => &mut stats.tasks_running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use nutsh_mockpc::MockPc;
    use nutsh_prism::Profile;

    const HOSTS: &str = "/clustermgmt/v4.3/config/hosts";

    /// A negotiated client on the mock, as `admin`.
    async fn client(pc: &MockPc) -> Client {
        let c = Client::connect(
            &Profile {
                host: pc.host(),
                port: pc.port(),
                username: "admin".into(),
                verify_tls: true,
                ca_bundle: None,
                plain_http: true,
            },
            "secret",
        )
        .unwrap();
        c.negotiate().await.unwrap();
        c
    }

    #[test]
    fn off_is_the_difference_and_only_when_both_are_known() {
        let mut s = Stats::default();
        assert_eq!(s.vms_off(), None);
        s.vms = Some(128);
        assert_eq!(s.vms_off(), None);
        s.vms_on = Some(121);
        assert_eq!(s.vms_off(), Some(7));
        // A server that reports more on than total never yields a negative.
        s.vms_on = Some(200);
        assert_eq!(s.vms_off(), Some(0));
    }

    #[test]
    fn a_missing_counter_shows_a_dash() {
        assert_eq!(show(None), "-");
        assert_eq!(show(Some(0)), "0");
    }

    /// Every counter names a top-level kind the catalog still has, one that can be listed with
    /// a bare `$limit=1`, and one that takes a `$filter` wherever a filter or a cluster scope
    /// is composed for it.
    #[test]
    fn every_counter_names_a_kind_the_catalog_has_and_can_filter() {
        assert_eq!(COUNTERS.len(), 7);
        for def in COUNTERS {
            let kind = nutsh_catalog::kind(def.kind).unwrap_or_else(|| panic!("{}", def.kind));
            assert!(kind.is_top_level(), "{}", def.kind);
            assert!(
                !kind.list_params.required,
                "{}: a bare $limit=1 must be legal",
                def.kind
            );
            if def.filter.is_some() || def.cluster_on.is_some() {
                assert!(kind.list_params.filter, "{} cannot be filtered", def.kind);
            }
        }
    }

    /// The Dashboard's "Running tasks" pane and the header's counter count one thing. The
    /// filter is written out twice - here and in `pages.toml` - so that neither the pane nor
    /// the block has to reach into the other; this is what stops the two spellings drifting.
    #[test]
    fn the_running_tasks_counter_and_the_dashboard_pane_share_one_filter() {
        let pane = nutsh_catalog::PAGES
            .iter()
            .find(|p| p.id == "dashboard")
            .expect("the Dashboard page")
            .panes
            .iter()
            .find(|p| p.kind == "prism.config.Task")
            .expect("the Running tasks pane");
        let counter = COUNTERS
            .iter()
            .find(|d| d.counter == Counter::TasksRunning)
            .expect("the running-tasks counter");
        assert_eq!(pane.filter, counter.filter);
    }

    /// Every counter writes its own field and nothing else's.
    #[test]
    fn each_counter_has_its_own_slot() {
        let mut stats = Stats::default();
        for (i, def) in COUNTERS.iter().enumerate() {
            *slot(&mut stats, def.counter) = Some(i as u64);
        }
        assert_eq!(
            [
                stats.clusters,
                stats.hosts,
                stats.vms,
                stats.vms_on,
                stats.alerts_critical,
                stats.alerts_warning,
                stats.tasks_running,
            ],
            [
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6)
            ]
        );
    }

    /// A quote in an extId would end the literal early and compose a filter the server rejects.
    #[test]
    fn a_quote_in_a_cluster_id_is_doubled_not_passed_through() {
        assert_eq!(odata_literal("0006158a-2f0d"), "0006158a-2f0d");
        assert_eq!(odata_literal("a' or true or '"), "a'' or true or ''");
    }

    /// What the server refuses is dropped; what it merely failed to answer is asked again.
    #[test]
    fn a_refusal_is_a_4xx_and_nothing_else() {
        assert!(is_refusal(&PrismError::NotFound("gone".into())));
        assert!(is_refusal(&PrismError::Forbidden("no".into())));
        assert!(is_refusal(&PrismError::Api {
            status: 400,
            message: "bad $filter".into(),
            retry_after: None
        }));
        assert!(!is_refusal(&PrismError::Api {
            status: 503,
            message: "down".into(),
            retry_after: None
        }));
        assert!(
            is_refusal(&PrismError::Auth),
            "a refused credential is refused on every cycle too"
        );
        assert!(!is_refusal(&PrismError::RateLimited {
            retry_after: Duration::from_secs(1)
        }));
        assert!(!is_refusal(&PrismError::Transport("reset".into())));
    }

    /// A whole cycle that fails keeps the previous values and sets `stale`, which draws the
    /// block dim; a cycle that asked nothing keeps nothing and dims nothing.
    #[test]
    fn a_failed_cycle_keeps_the_previous_values_and_marks_them_stale() {
        let previous = Stats {
            clusters: Some(1),
            vms: Some(3),
            ..Default::default()
        };
        assert_eq!(
            fold(previous, Stats::default(), Outcome::AllFailed),
            Stats {
                stale: true,
                ..previous
            }
        );
        assert_eq!(
            fold(previous, Stats::default(), Outcome::NothingAsked),
            Stats::default(),
            "no request went out, so there is nothing to call stale"
        );
        let fresh = Stats {
            clusters: Some(2),
            ..Default::default()
        };
        assert_eq!(fold(previous, fresh, Outcome::Answered), fresh);
    }

    /// One cycle against a healthy Prism Central. The mock ignores `$filter` and reports the
    /// unfiltered total, so the filtered counters read the same number as their unfiltered
    /// siblings; what is asserted here is that every one of them answered.
    #[tokio::test]
    async fn a_healthy_cycle_fills_every_counter() {
        let pc = MockPc::builder().start().await;
        let client = client(&pc).await;
        let mut refused = Vec::new();
        let (stats, outcome) = cycle(&client, None, &Stats::default(), &mut refused).await;
        assert_eq!(outcome, Outcome::Answered);
        assert_eq!(stats.clusters, Some(1));
        assert_eq!(stats.hosts, Some(1));
        assert_eq!(stats.vms, Some(3));
        assert!(stats.tasks_running.is_some());
        assert!(!stats.stale);
        assert!(refused.is_empty());
    }

    /// A counter whose request came back 5xx keeps the number it had, and the cycle is not a
    /// failed one: the counters that did answer are live.
    #[tokio::test]
    async fn a_transient_failure_carries_one_counter_forward() {
        let pc = MockPc::builder().fail_path(HOSTS, 503).start().await;
        let client = client(&pc).await;
        let previous = Stats {
            hosts: Some(9),
            ..Default::default()
        };
        let mut refused = Vec::new();
        let (stats, outcome) = cycle(&client, None, &previous, &mut refused).await;
        assert_eq!(outcome, Outcome::Answered);
        assert_eq!(stats.hosts, Some(9), "the previous number is carried");
        assert_eq!(stats.vms, Some(3), "the others are unaffected");
        assert!(refused.is_empty(), "a 503 is not a refusal");
    }

    /// A counter the server refuses blanks - the previous number is not carried over a verdict
    /// that will be the same in thirty seconds - and is not asked a second time.
    #[tokio::test]
    async fn a_refused_counter_blanks_and_is_dropped_from_the_cycle() {
        let pc = MockPc::builder().missing_path(HOSTS).start().await;
        let client = client(&pc).await;
        let previous = Stats {
            hosts: Some(9),
            ..Default::default()
        };
        let mut refused = Vec::new();
        let (stats, outcome) = cycle(&client, None, &previous, &mut refused).await;
        assert_eq!(outcome, Outcome::Answered);
        assert_eq!(stats.hosts, None);
        assert_eq!(refused, vec![Counter::Hosts]);
        let asked = pc.requests_to("/config/hosts").len();
        let (again, _) = cycle(&client, None, &stats, &mut refused).await;
        assert_eq!(again.hosts, None);
        assert_eq!(
            pc.requests_to("/config/hosts").len(),
            asked,
            "the refusal is not repeated every cycle"
        );
    }

    /// A Prism Central that serves none of these namespaces issues no request at all: the
    /// block reads `-` and is not dimmed, because there is nothing behind it to go stale.
    #[tokio::test]
    async fn a_prism_central_that_serves_none_of_them_asks_nothing() {
        let pc = MockPc::builder()
            .unavailable_namespace("clustermgmt")
            .unavailable_namespace("vmm")
            .unavailable_namespace("monitoring")
            .unavailable_namespace("prism")
            .start()
            .await;
        let client = client(&pc).await;
        let previous = Stats {
            vms: Some(7),
            ..Default::default()
        };
        let (stats, outcome) = cycle(&client, None, &previous, &mut Vec::new()).await;
        assert_eq!(outcome, Outcome::NothingAsked);
        assert_eq!(stats, Stats::default(), "every slot blanked, none dimmed");
        assert_eq!(fold(previous, stats, outcome), Stats::default());
    }

    /// A pinned cluster scopes what can be scoped, blanks what cannot, and knows without
    /// asking that it is looking at one cluster.
    #[tokio::test]
    async fn a_pinned_cluster_scopes_what_it_can_and_blanks_what_it_cannot() {
        let pc = MockPc::builder().start().await;
        let client = client(&pc).await;
        let cluster = "0006158a-2f0d-4d5a-8e2d-000000000010";
        let (stats, outcome) =
            cycle(&client, Some(cluster), &Stats::default(), &mut Vec::new()).await;
        assert_eq!(outcome, Outcome::Answered);
        assert_eq!(
            stats.clusters,
            Some(1),
            "the pin is one cluster by definition"
        );
        assert_eq!(
            stats.hosts, None,
            "no filter spelling this project has seen"
        );
        assert_eq!(stats.tasks_running, None, "clusterExtIds is an array");
        assert!(
            pc.requests_to("/ahv/config/vms")
                .iter()
                .any(|r| r
                    .query
                    .iter()
                    .any(|(k, v)| k == "$filter"
                        && v.contains(&format!("cluster/extId eq '{cluster}'")))),
            "the VM counters are scoped to the pin"
        );
    }
}
