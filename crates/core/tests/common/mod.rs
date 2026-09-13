/// The one entity every fake row is, matching the id the mock fixtures use.
#[allow(dead_code)]
pub const WEB01: &str = "3d0c4a2e-1b8f-4c1a-9e2f-000000000001";

use nutsh_mockpc::MockPc;
use nutsh_prism::{Client, Profile};

/// A negotiated client on the mock, as `admin`.
pub async fn client(pc: &MockPc) -> Client {
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

/// A client that has **not** negotiated, with a password of the caller's choosing: what a test
/// about a refused credential needs, because negotiating is itself a request and a wrong
/// password would end it before the test began.
///
/// The allow is because this module is compiled into each of the several test binaries that
/// share it, and only one of them uses this.
#[allow(dead_code)]
pub fn raw_client(pc: &MockPc, password: &str) -> Client {
    Client::connect(
        &Profile {
            host: pc.host(),
            port: pc.port(),
            username: "admin".into(),
            verify_tls: true,
            ca_bundle: None,
            plain_http: true,
        },
        password,
    )
    .unwrap()
}

/// A [`Source`] with no socket under it, for the tests that pause the clock.
///
/// `tokio::time::pause` auto-advances the clock whenever the runtime has nothing to do, and a
/// runtime waiting on a socket has nothing to do — so a paused test against `MockPc` advances
/// into `reqwest`'s own connect timeout and fails with `operation timed out` on loopback. A
/// scheduler test is about *when* a request is made, not about how it travels, so this answers
/// from memory and leaves the scheduler's timers as the only ones the clock can advance to.
///
/// Anything about the wire — paging, ETags, the auth valve, rate limiting — belongs in a test
/// that uses the real client and a real clock.
#[allow(dead_code)]
#[derive(Default)]
pub struct FakeSource {
    lists: std::sync::atomic::AtomicUsize,
    gets: std::sync::atomic::AtomicUsize,
    /// Every call from now on is a refused credential.
    refuse: std::sync::atomic::AtomicBool,
    metrics: nutsh_prism::Metrics,
}

#[allow(dead_code)]
impl FakeSource {
    pub fn new() -> std::sync::Arc<FakeSource> {
        std::sync::Arc::new(FakeSource::default())
    }

    /// Every later `list_page` and `get_in` answers `PrismError::Auth`.
    pub fn refuse_credential(&self) {
        self.refuse.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// How many list requests the scheduler has made. The counterpart of
    /// `MockPc::requests_to`, and the whole reason this exists: a paused test asserts on a
    /// count, not on a round trip.
    pub fn lists(&self) -> usize {
        self.lists.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn gets(&self) -> usize {
        self.gets.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn refused(&self) -> bool {
        self.refuse.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// One row, enough for a table to have something in it.
    fn row(kind: &'static nutsh_catalog::Kind) -> nutsh_prism::Entity {
        nutsh_prism::Entity::new(
            kind,
            serde_json::json!({ kind.ext_id_key: WEB01, "name": "web-01" }),
            None,
        )
    }
}

impl nutsh_core::source::Source for FakeSource {
    fn list_page<'a>(
        &'a self,
        kind: &'static nutsh_catalog::Kind,
        page: u32,
        _opts: &'a nutsh_prism::ListOptions,
    ) -> futures::future::BoxFuture<'a, Result<nutsh_prism::Page, nutsh_prism::PrismError>> {
        self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let refused = self.refused();
        Box::pin(async move {
            if refused {
                return Err(nutsh_prism::PrismError::Auth);
            }
            // One page, and it is the last: `total` equals what page 0 carries, so a walk ends
            // where it starts and a test counts cycles rather than pages.
            Ok(nutsh_prism::Page {
                entities: if page == 0 {
                    vec![FakeSource::row(kind)]
                } else {
                    Vec::new()
                },
                total: Some(1),
            })
        })
    }

    fn get_in<'a>(
        &'a self,
        kind: &'static nutsh_catalog::Kind,
        _parents: &'a [String],
        _ext_id: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<nutsh_prism::Entity, nutsh_prism::PrismError>> {
        self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let refused = self.refused();
        Box::pin(async move {
            if refused {
                return Err(nutsh_prism::PrismError::Auth);
            }
            Ok(FakeSource::row(kind))
        })
    }

    fn metrics(&self) -> &nutsh_prism::Metrics {
        &self.metrics
    }

    fn mark_missing(&self, _kind: &nutsh_catalog::Kind) {}
    fn forget_missing(&self, _kind: &nutsh_catalog::Kind) {}
}
