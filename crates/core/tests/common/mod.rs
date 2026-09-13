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
