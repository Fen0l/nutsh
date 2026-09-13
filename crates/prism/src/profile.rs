//! Connection profile (everything except the password).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub verify_tls: bool,
    pub ca_bundle: Option<PathBuf>,
    /// Plain HTTP, for the mock server in tests only.
    pub plain_http: bool,
}

impl Profile {
    pub fn base_url(&self) -> String {
        let scheme = if self.plain_http { "http" } else { "https" };
        format!("{scheme}://{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_uses_https_by_default() {
        let p = Profile {
            host: "pc.lab".into(),
            port: 9440,
            username: "admin".into(),
            verify_tls: true,
            ca_bundle: None,
            plain_http: false,
        };
        assert_eq!(p.base_url(), "https://pc.lab:9440");
        let p = Profile {
            plain_http: true,
            ..p
        };
        assert_eq!(p.base_url(), "http://pc.lab:9440");
    }
}
