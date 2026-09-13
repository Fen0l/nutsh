//! OS keyring backend: macOS login Keychain by default, Linux Secret Service behind the
//! `linux-keyring` feature. Any failure other than "no entry" marks the backend unavailable so
//! the facade falls through to the file store.

use std::collections::HashMap;
use std::sync::OnceLock;

use keyring_core::{Entry, Error};

use crate::error::ConfigError;
use crate::secrets::SecretStore;

const SERVICE: &str = "nutsh";

/// The attribute each platform store matches the account on in `Entry::search`: the macOS
/// store parses only `service`/`user`, the Secret Service store stores it as `username`.
#[cfg(target_os = "macos")]
const USER_ATTR: &str = "user";
#[cfg(all(target_os = "linux", feature = "linux-keyring"))]
const USER_ATTR: &str = "username";

pub struct KeyringStore(());

/// Installed once per process; `Err` remembers why the platform store could not be created.
static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();

impl KeyringStore {
    /// `None` when the platform store cannot be created (no daemon, no keychain access).
    pub fn new() -> Option<KeyringStore> {
        let installed = INSTALLED.get_or_init(|| {
            #[cfg(target_os = "macos")]
            let store = apple_native_keyring_store::keychain::Store::new();
            #[cfg(all(target_os = "linux", feature = "linux-keyring"))]
            let store = dbus_secret_service_keyring_store::Store::new();
            match store {
                Ok(s) => {
                    keyring_core::set_default_store(s);
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        });
        installed.is_ok().then_some(KeyringStore(()))
    }
}

fn unavailable(e: Error) -> ConfigError {
    ConfigError::SecretUnavailable(e.to_string())
}

impl SecretStore for KeyringStore {
    fn name(&self) -> &'static str {
        "keyring"
    }

    fn get(&self, key: &str) -> Result<Option<String>, ConfigError> {
        let entry = Entry::new(SERVICE, key).map_err(unavailable)?;
        match entry.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), ConfigError> {
        Entry::new(SERVICE, key)
            .map_err(unavailable)?
            .set_password(value)
            .map_err(unavailable)
    }

    fn delete(&self, key: &str) -> Result<(), ConfigError> {
        let entry = Entry::new(SERVICE, key).map_err(unavailable)?;
        match entry.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(unavailable(e)),
        }
    }

    /// Searches on attributes instead of reading the password, so `ctx list` cannot raise one
    /// Keychain unlock prompt per context. A store with no search support falls back to the
    /// trait's reading default rather than reporting the backend as broken.
    fn exists(&self, key: &str) -> Result<bool, ConfigError> {
        let spec = HashMap::from([("service", SERVICE), (USER_ATTR, key)]);
        match Entry::search(&spec) {
            Ok(found) => Ok(!found.is_empty()),
            Err(Error::NoEntry) => Ok(false),
            Err(Error::NotSupportedByStore(_)) => self.get(key).map(|v| v.is_some()),
            Err(e) => Err(unavailable(e)),
        }
    }
}
