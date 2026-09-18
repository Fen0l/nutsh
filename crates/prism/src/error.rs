//! Error type surfaced to callers. Messages never contain credentials.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use rustls::CertificateError;

#[derive(Debug, thiserror::Error)]
pub enum PrismError {
    #[error("TLS configuration error: {0}")]
    Tls(String),
    /// The server's certificate failed verification. Kept apart from `Connect` because what to
    /// do about it depends on the reason, and the binary has to be able to say so.
    #[error("TLS certificate of {host} rejected: {reason}")]
    Certificate {
        host: String,
        reason: CertificateReason,
    },
    #[error("cannot connect to {host}: {message}")]
    Connect { host: String, message: String },
    #[error("authentication failed (HTTP 401)")]
    Auth,
    /// A 401 from one endpoint while the session it rode still answers elsewhere: this
    /// account may not read that endpoint. Not a refused credential - nothing was presented
    /// to reach this verdict, and nothing latches on it.
    #[error("not permitted (HTTP 401): {0}")]
    Denied(String),
    #[error("forbidden (HTTP 403): {0}")]
    Forbidden(String),
    #[error("not found (HTTP 404): {0}")]
    NotFound(String),
    #[error("{0}")]
    Ambiguous(String),
    /// A name that matched nothing locally. Distinct from `NotFound`, which is an HTTP 404.
    #[error("{0}")]
    Unresolved(String),
    #[error(
        "precondition failed (HTTP 412): {kind} {ext_id} changed since it was loaded (action {action})"
    )]
    Conflict {
        kind: &'static str,
        ext_id: String,
        action: String,
    },
    #[error("rate limited (HTTP 429), retry after {}s", .retry_after.as_secs())]
    RateLimited { retry_after: Duration },
    /// Any status `ok` does not name: a 5xx the service answered with, a 400 for a parameter
    /// this client got wrong, a 405. `retry_after` is the header's when the far end sent one -
    /// a 503 under load routinely does - and it is what a poller waits before asking again.
    #[error("HTTP {status}: {message}")]
    Api {
        status: u16,
        message: String,
        retry_after: Option<Duration>,
    },
    #[error("cannot decode response: {0}")]
    Decode(String),
    #[error("catalog: {0}")]
    Catalog(String),
    #[error("request failed: {0}")]
    Transport(String),
}

impl PrismError {
    /// How long the far end asked to be left alone, when it asked. `None` is "it did not
    /// say", which is not the same as zero: the caller's own backoff decides then.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            PrismError::RateLimited { retry_after } => Some(*retry_after),
            PrismError::Api { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// The server refused this account, not this request. Callers act on it - a 403 disproves
    /// whatever can-i last answered - so the test is a method rather than a match on the
    /// `Display` text: rewording the message above must not silently disable them.
    pub fn is_forbidden(&self) -> bool {
        matches!(self, PrismError::Forbidden(_))
    }
}

/// Why a peer certificate was rejected, coarse enough to choose advice on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateReason {
    /// The chain does not end in a trusted root: self-signed, or a private CA.
    UnknownIssuer,
    /// The server presented a CA certificate as its own. Prism Central's default self-signed
    /// certificate carries `CA:TRUE`, and rustls refuses such a certificate as an end entity
    /// even when it sits in the trust store, so a CA bundle cannot help here.
    CaUsedAsEndEntity,
    NotValidForName,
    Expired,
    Other(String),
}

impl fmt::Display for CertificateReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownIssuer => f.write_str("its issuer is not trusted"),
            Self::CaUsedAsEndEntity => f.write_str(
                "the server presented a CA certificate as its own (Prism Central's default self-signed certificate does this)",
            ),
            Self::NotValidForName => f.write_str("it is not valid for this host name"),
            Self::Expired => f.write_str("it has expired"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl From<&CertificateError> for CertificateReason {
    fn from(e: &CertificateError) -> Self {
        match e {
            CertificateError::UnknownIssuer => Self::UnknownIssuer,
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                Self::NotValidForName
            }
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => Self::Expired,
            // webpki's own verdicts arrive opaque; their Display is the variant name.
            CertificateError::Other(o) => match o.to_string().as_str() {
                "CaUsedAsEndEntity" => Self::CaUsedAsEndEntity,
                other => Self::Other(other.to_string()),
            },
            other => Self::Other(other.to_string()),
        }
    }
}

/// The certificate rejection in `e`'s source chain, if that is what failed. reqwest reports a
/// failed handshake as a plain connect error, so the rustls verdict has to be dug out of the
/// `io::Error`s tokio-rustls and reqwest's connector wrap around it.
pub fn certificate_error(e: &(dyn Error + 'static)) -> Option<CertificateReason> {
    let mut current = Some(e);
    while let Some(err) = current {
        if let Some(rustls::Error::InvalidCertificate(ce)) =
            unwrap_io(err).downcast_ref::<rustls::Error>()
        {
            return Some(CertificateReason::from(ce));
        }
        current = err.source();
    }
    None
}

/// Peels nested `io::Error`s down to the error they carry. `io::Error::source` yields the
/// wrapped error's own source and skips the wrapped error itself, so a plain chain walk never
/// sees it; `get_ref` does.
fn unwrap_io<'a>(e: &'a (dyn Error + 'static)) -> &'a (dyn Error + 'static) {
    let mut e = e;
    while let Some(inner) = e
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
    {
        e = inner;
    }
    e
}

/// An error and its whole source chain, joined: "error sending request: invalid peer certificate: UnknownIssuer".
pub fn describe(e: &dyn std::error::Error) -> String {
    let mut parts = vec![e.to_string()];
    let mut current = e.source();
    while let Some(s) = current {
        let text = s.to_string();
        if parts.last() != Some(&text) {
            parts.push(text);
        }
        current = s.source();
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Inner;

    impl std::fmt::Display for Inner {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("inner")
        }
    }

    impl std::error::Error for Inner {}

    #[derive(Debug)]
    struct Outer(Inner);

    impl std::fmt::Display for Outer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("outer")
        }
    }

    impl std::error::Error for Outer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn describe_joins_the_source_chain() {
        assert_eq!(describe(&Outer(Inner)), "outer: inner");
        assert_eq!(describe(&Inner), "inner");
    }

    #[test]
    fn rate_limited_reports_seconds() {
        let e = PrismError::RateLimited {
            retry_after: Duration::from_secs(30),
        };
        assert_eq!(e.to_string(), "rate limited (HTTP 429), retry after 30s");
    }

    #[derive(Debug)]
    struct Wrapped(std::io::Error);
    impl fmt::Display for Wrapped {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("client error (Connect)")
        }
    }
    impl Error for Wrapped {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    /// Stands in for `webpki::Error`, whose Display is its variant name.
    #[derive(Debug)]
    struct Verdict(&'static str);
    impl fmt::Display for Verdict {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }
    impl Error for Verdict {}

    /// The shape reqwest hands back, as probed against a local TLS server: an outer error whose
    /// source is reqwest's `io::Error` around tokio-rustls's `io::Error` around the verdict.
    fn handshake(ce: CertificateError) -> Wrapped {
        let tokio_rustls = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(ce),
        );
        Wrapped(std::io::Error::other(tokio_rustls))
    }

    fn verdict(name: &'static str) -> CertificateError {
        CertificateError::Other(rustls::OtherError(std::sync::Arc::new(Verdict(name))))
    }

    #[test]
    fn certificate_error_is_dug_out_of_the_io_error() {
        assert_eq!(
            certificate_error(&handshake(CertificateError::UnknownIssuer)),
            Some(CertificateReason::UnknownIssuer)
        );
        assert_eq!(
            certificate_error(&handshake(verdict("CaUsedAsEndEntity"))),
            Some(CertificateReason::CaUsedAsEndEntity)
        );
        assert_eq!(
            certificate_error(&handshake(CertificateError::Expired)),
            Some(CertificateReason::Expired)
        );
        assert_eq!(
            certificate_error(&handshake(verdict("BadSignature"))),
            Some(CertificateReason::Other("BadSignature".into()))
        );
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        assert_eq!(certificate_error(&Wrapped(refused)), None);
    }

    #[test]
    fn certificate_variant_names_host_and_reason() {
        let e = PrismError::Certificate {
            host: "pc.lab".into(),
            reason: CertificateReason::NotValidForName,
        };
        assert_eq!(
            e.to_string(),
            "TLS certificate of pc.lab rejected: it is not valid for this host name"
        );
    }
}
