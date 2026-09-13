//! What the scheduler needs from a Prism Central, and nothing else.
//!
//! The scheduler is a clock: it decides when to ask, how far to walk and when to stop. Holding a
//! `nutsh_prism::Client` directly tied that clock to a socket, and a test of the timing had to
//! stand up an HTTP stack to exercise it — which cannot be done under `tokio::time::pause`,
//! because a paused runtime waiting on a socket looks idle and auto-advances the clock into the
//! request's own timeout.
//!
//! Four methods, because four is all the scheduler calls.

use std::sync::Arc;

use futures::future::BoxFuture;
use nutsh_catalog::Kind;
use nutsh_prism::{Client, Entity, ListOptions, Metrics, Page, PrismError};

/// The scheduler's view of a Prism Central.
///
/// `list_page` hands back a boxed future rather than being an `async fn`, so the trait stays
/// object-safe and the scheduler can hold one `Arc<dyn Source>` instead of a type parameter
/// that every caller, up to the TUI, would have to name.
pub trait Source: Send + Sync {
    fn list_page<'a>(
        &'a self,
        kind: &'static Kind,
        page: u32,
        opts: &'a ListOptions,
    ) -> BoxFuture<'a, Result<Page, PrismError>>;

    /// One entity, for a single-entity subscription: the task watch and the detail pane.
    fn get_in<'a>(
        &'a self,
        kind: &'static Kind,
        parents: &'a [String],
        ext_id: &'a str,
    ) -> BoxFuture<'a, Result<Entity, PrismError>>;

    fn metrics(&self) -> &Metrics;

    /// This kind's list answered 404: remember it for every surface that asks, not only for the
    /// table that paid for it.
    fn mark_missing(&self, kind: &Kind);

    /// A cycle completed, so the 404 is no longer the standing answer.
    fn forget_missing(&self, kind: &Kind);
}

impl Source for Client {
    fn list_page<'a>(
        &'a self,
        kind: &'static Kind,
        page: u32,
        opts: &'a ListOptions,
    ) -> BoxFuture<'a, Result<Page, PrismError>> {
        Box::pin(Client::list_page(self, kind, page, opts))
    }

    fn get_in<'a>(
        &'a self,
        kind: &'static Kind,
        parents: &'a [String],
        ext_id: &'a str,
    ) -> BoxFuture<'a, Result<Entity, PrismError>> {
        Box::pin(Client::get_in(self, kind, parents, ext_id))
    }

    fn metrics(&self) -> &Metrics {
        Client::metrics(self)
    }

    fn mark_missing(&self, kind: &Kind) {
        Client::mark_missing(self, kind);
    }

    fn forget_missing(&self, kind: &Kind) {
        Client::forget_missing(self, kind);
    }
}

/// An `Arc<Client>` is an `Arc<dyn Source>`; this names the coercion for callers that need it
/// spelled out.
pub fn from_client(client: Arc<Client>) -> Arc<dyn Source> {
    client
}
