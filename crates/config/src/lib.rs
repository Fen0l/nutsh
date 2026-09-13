//! Config file, contexts, and secret storage for nutsh.
//!
//! A *context* names one Prism Central (host, port, username, TLS settings) and may pin a
//! cluster. Passwords never live in the config file; see [`secrets`].

pub mod atomic;
pub mod error;
pub mod file;
pub mod model;
pub mod paths;
pub mod secrets;

#[cfg(any(
    target_os = "macos",
    all(target_os = "linux", feature = "linux-keyring")
))]
pub mod keyring_store;

pub use error::ConfigError;
pub use file::{load, save};
pub use model::{
    Config, Context, DEFAULT_PORT, Header, Interval, LogLevel, Nav, Refresh, RuleSpec, Word,
    secret_key, valid_name,
};
pub use paths::{cache_dir, config_path, logs_dir, secrets_dir};
pub use secrets::{FileStore, MemoryStore, SecretStore, Secrets};
