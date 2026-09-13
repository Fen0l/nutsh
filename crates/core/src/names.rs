//! The name cache's warm-up: one page of each reference kind at connect, so a `Reference` cell
//! resolves before anything has listed the kind it points at.
//!
//! `Names` is otherwise filled in exactly two places, both in `Store::apply`: the rows of a
//! table that finished polling, and the one entity a single-entity subscription fetched. So a
//! reference resolves, without this, if and only if the referenced entity has already come back as a
//! row of some table this session polled - which is why the Dashboard's VM pane can name a
//! cluster where `nutsh vm --snapshot`, whose only subscription is VMs, shows a bare ext id.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nutsh_catalog::KINDS;
use nutsh_prism::{Client, ListOptions};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::scheduler::Msg;

/// Between cycles. Five minutes rather than never, because a cluster added mid-session should
/// not stay a UUID for the rest of the day; five minutes rather than thirty seconds, because
/// names change about as often as clusters are built. The stats poller's rhythm is not reused:
/// it is seven `$limit=1` counters and this is ten full pages, and coupling them would make the
/// header's counters wait on a page of categories.
const WARM_PERIOD: Duration = Duration::from_secs(300);

/// Between requests inside one cycle: the pacing `sampler.rs` already uses, so ten gets never
/// look like a burst to a rate limiter.
const PACE: Duration = Duration::from_millis(200);

/// One page per warmed kind, page 0 only. `list_page`, not `list_all`: a second page of a
/// reference kind is not worth a second request at connect.
const WARM_LIMIT: u32 = 100;

/// The warm-up task. `Live` holds the handle and `impl Drop for Live` aborts it: `App::attach`
/// replaces `Live` wholesale on a context switch, so a bare `tokio::spawn` would outlive the
/// session and drop the previous Prism Central's names into the new store. Belt and braces
/// beside the abort - which races a message already in the channel - `Msg::Names` carries the
/// session generation and `App::apply` drops a message whose generation is not the current one.
///
/// `idle` is the scheduler's own flag, on the same terms as the Disaster Recovery sampler's:
/// this is a bare `tokio::spawn` rather than a `Subscription`, so it has no task of the
/// scheduler's to consult it and takes the flag itself. A cycle is one page of every warmed
/// kind for a cache nobody is reading a reference out of, and the pause would be a claim about
/// the request rate rather than a fact without it. A skipped cycle costs nothing on the way
/// back: the tables fill the same cache, and `App::woke` walks every one of them.
pub fn spawn(
    client: Arc<Client>,
    tx: mpsc::Sender<Msg>,
    generation: u64,
    idle: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if !idle.load(Ordering::Relaxed) {
                let names = cycle(&client).await;
                if tx.send(Msg::Names { generation, names }).await.is_err() {
                    return;
                }
            }
            // Both exits, read together and *after* the cycle rather than before it.
            //
            // The receiver having gone away: while the pause holds there is no send, so
            // without this a closed channel would go unnoticed until somebody came back to the
            // keyboard. The abort in `impl Drop for Live` is the usual end.
            //
            // And a credential this Prism Central refused, which is the cycle above's to
            // learn. This is not a subscription, so nothing of the scheduler's would ever stop
            // it: ten kinds a cycle, every five minutes, each one a password on the wire, for
            // a cache nobody is reading a reference out of.
            if tx.is_closed() || client.auth_rejected() {
                return;
            }
            tokio::time::sleep(WARM_PERIOD).await;
        }
    })
}

/// One pass over the warmed kinds, sequential and paced.
///
/// A kind whose namespace this Prism Central does not serve is skipped without a request; a
/// kind whose request fails is skipped without a retry, because the next cycle asks again and a
/// table that lists the kind fills the same cache meanwhile.
async fn cycle(client: &Client) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut asked = false;
    for kind in KINDS.iter().filter(|k| k.warm) {
        if !client.is_served(kind) {
            continue;
        }
        if asked {
            tokio::time::sleep(PACE).await;
        }
        asked = true;
        let opts = ListOptions {
            limit: WARM_LIMIT,
            ..Default::default()
        };
        let page = match client.list_page(kind, 0, &opts).await {
            Ok(page) => page,
            // A refused credential is refused for every kind after this one too, and asking is
            // exactly what must not happen. The names already gathered are still worth sending.
            Err(nutsh_prism::PrismError::Auth) => break,
            // Anything else is skipped without a retry: the next cycle asks again, and a table
            // that lists the kind fills the same cache meanwhile.
            Err(_) => continue,
        };
        out.extend(
            page.entities
                .into_iter()
                // An entity that named itself with its own extId has nothing to teach the
                // cache, and `id -> id` would make the cell print 36 characters where it
                // prints an 8-character stub today. The generator refuses `warm` on a kind
                // where that is the *rule*; this drops the individual row that has no name.
                .filter(|e| !e.ext_id.is_empty() && e.name != e.ext_id)
                .map(|e| (e.ext_id, e.name)),
        );
    }
    out
}
