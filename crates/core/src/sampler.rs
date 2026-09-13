//! The Disaster Recovery page's bounded sampler.
//!
//! There is no protected-resources *list*: `GET /dataprotection/{v}/config/protected-resources/
//! {extId}` is get-by-id only, which is why the kind is absent from the catalog, and
//! `replicationStates[].replicationStatus` is reachable one entity at a time. So the sampler
//! takes at most `CAP` VMs, asks about them one at a time with a pause between, and labels
//! itself `Sampled VMs N` so the number never claims to be a census.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nutsh_prism::{Client, ListOptions, PrismError};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::scheduler::Msg;
use crate::stats::Count;

/// How many VMs one cycle asks about.
const CAP: u32 = 50;
/// Between requests, so fifty gets never look like a burst to a rate limiter.
const SPACING: Duration = Duration::from_millis(200);
/// Between cycles.
const PERIOD: Duration = Duration::from_secs(300);
/// The dataprotection kind the catalog does name, asked whether this Prism Central serves the
/// namespace at all: `protected-resources` has no list endpoint and so no kind of its own.
const PROBE: &str = "dataprotection.config.RecoveryPoint";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProtectionSample {
    pub policies: Count,
    pub plans: Count,
    pub recovery_points: Count,
    pub jobs: Count,
    /// How many VMs this cycle got an answer about; a request that failed counts for nothing.
    pub sampled: u64,
    /// VMs, not replication links: one VM replicating to two targets counts once, at its worst
    /// target, so the three tallies are a breakdown of `sampled` and never sum above it.
    pub in_sync: u64,
    pub syncing: u64,
    pub out_of_sync: u64,
    /// The endpoint answered at all. `false` blanks the sampled lines and leaves the counters:
    /// a Prism Central that does not serve `dataprotection` is never asked, so the block
    /// dashes rather than reporting three zeroes as if nothing were protected.
    pub available: bool,
}

/// One cycle every five minutes until the handle is aborted.
///
/// `idle` is the scheduler's own flag, because this poller is not a `Subscription` and so has
/// no task of the scheduler's to consult it. A cycle it skips is fifty-odd requests nobody was
/// going to read; it skips the work rather than the sleep, so the page is one period behind
/// when somebody comes back rather than a cycle's worth of requests ahead of them.
pub fn spawn(
    client: Arc<Client>,
    tx: mpsc::Sender<Msg>,
    generation: u64,
    idle: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if !idle.load(Ordering::Relaxed) {
                let sample = cycle(&client).await;
                if tx.send(Msg::Sample { generation, sample }).await.is_err() {
                    return;
                }
            }
            // A credential this Prism Central refused ends the sampler outright, in the cycle
            // that learns it: a cycle is four counters and up to fifty gets, and not one of
            // them can be answered.
            if client.auth_rejected() {
                return;
            }
            tokio::time::sleep(PERIOD).await;
        }
    })
}

async fn cycle(client: &Client) -> ProtectionSample {
    let mut sample = ProtectionSample {
        policies: count(client, "datapolicies.config.ProtectionPolicy").await,
        plans: count(client, "datapolicies.config.RecoveryPlan").await,
        recovery_points: count(client, "dataprotection.config.RecoveryPoint").await,
        jobs: count(client, "dataprotection.config.RecoveryPlanJob").await,
        ..ProtectionSample::default()
    };
    // The gate `count` gives itself, for a path that has no kind to carry one. Without it every
    // GET below 404s on a Prism Central that does not serve `dataprotection`, and a 404 there
    // reads as "this VM is not protected" - the block would report three zeroes as fact for a
    // question this Prism Central cannot answer, and spend fifty requests learning nothing.
    let Some(probe) = nutsh_catalog::kind(PROBE) else {
        return sample;
    };
    if !client.is_served(probe) {
        return sample;
    }
    let Some(version) = nutsh_catalog::namespace("dataprotection").map(|n| n.version) else {
        return sample;
    };
    for ext_id in vm_ids(client).await {
        let path = format!("/dataprotection/{version}/config/protected-resources/{ext_id}");
        match client.get_path(&path).await {
            Ok(value) => {
                sample.available = true;
                tally(&value, &mut sample);
            }
            // Not protected: inside a served namespace a 404 here is an answer, not a failure.
            Err(PrismError::NotFound(_)) => sample.available = true,
            // Anything else - the account may not read it, the service is down - ends the
            // cycle: fifty repetitions of the same refusal is fifty requests wasted, and a VM
            // nobody could ask about is not a VM this block sampled.
            Err(_) => return sample,
        }
        sample.sampled += 1;
        tokio::time::sleep(SPACING).await;
    }
    sample
}

/// A kind's total, read the way the header's counters are: `$limit=1` and the response's
/// `metadata.totalAvailableResults`.
async fn count(client: &Client, id: &str) -> Count {
    let kind = nutsh_catalog::kind(id)?;
    if !client.is_served(kind) {
        return None;
    }
    let opts = ListOptions {
        limit: 1,
        ..Default::default()
    };
    client.list_page(kind, 0, &opts).await.ok()?.total
}

/// At most `CAP` VM extIds, in one request.
async fn vm_ids(client: &Client) -> Vec<String> {
    let Some(kind) = nutsh_catalog::kind("vmm.ahv.config.Vm") else {
        return Vec::new();
    };
    if !client.is_served(kind) {
        return Vec::new();
    }
    let opts = ListOptions {
        limit: CAP,
        ..Default::default()
    };
    match client.list_page(kind, 0, &opts).await {
        Ok(page) => page.entities.into_iter().map(|e| e.ext_id).collect(),
        Err(_) => Vec::new(),
    }
}

/// One VM's worst replication state, counted once.
///
/// The tallies sit under `Sampled VMs N` and read as a breakdown of those N, so a VM that
/// replicates to two targets must not add two to them: it is out of sync if any target is,
/// syncing if any is, and in sync only when every target says so.
fn tally(value: &Value, sample: &mut ProtectionSample) {
    let mut worst = 0;
    for state in crate::path::get(value, "replicationStates[].replicationStatus") {
        let rank = match crate::status::normalize(state.as_str().unwrap_or_default()).as_str() {
            "IN_SYNC" => 1,
            "SYNCING" => 2,
            "OUT_OF_SYNC" => 3,
            // `$UNKNOWN` and `$REDACTED` are both declared on `replicationStatus`, and neither
            // is a state worth counting as a failure.
            _ => 0,
        };
        worst = worst.max(rank);
    }
    match worst {
        1 => sample.in_sync += 1,
        2 => sample.syncing += 1,
        3 => sample.out_of_sync += 1,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_vm_is_tallied_once_at_its_worst_target() {
        let mut s = ProtectionSample::default();
        tally(
            &json!({"replicationStates": [
                {"replicationStatus": "IN_SYNC"},
                {"replicationStatus": "OUT_OF_SYNC"},
                {"replicationStatus": "$UNKNOWN"},
                {"replicationStatus": "syncing"}
            ]}),
            &mut s,
        );
        assert_eq!(
            (s.in_sync, s.syncing, s.out_of_sync),
            (0, 0, 1),
            "one VM, one tally, at the worst of its four targets"
        );
        tally(
            &json!({"replicationStates": [
                {"replicationStatus": "IN_SYNC"},
                {"replicationStatus": "in-sync"}
            ]}),
            &mut s,
        );
        assert_eq!((s.in_sync, s.syncing, s.out_of_sync), (1, 0, 1));
        // Neither a resource without states nor one whose states say nothing worth counting
        // moves a tally: the three lines would then sum above the VMs they sit under.
        tally(&json!({}), &mut s);
        tally(
            &json!({"replicationStates": [{"replicationStatus": "$UNKNOWN"}]}),
            &mut s,
        );
        assert_eq!((s.in_sync, s.syncing, s.out_of_sync), (1, 0, 1));
    }
}
