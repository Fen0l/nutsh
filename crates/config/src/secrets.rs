//! Where passwords live: an ordered set of backends behind one trait.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::ConfigError;

pub trait SecretStore: Send + Sync {
    fn name(&self) -> &'static str;
    fn get(&self, key: &str) -> Result<Option<String>, ConfigError>;
    fn set(&self, key: &str, value: &str) -> Result<(), ConfigError>;
    fn delete(&self, key: &str) -> Result<(), ConfigError>;

    /// Is a password stored under `key`? Callers that only need to *say* where a password
    /// lives - `ctx list` - must use this rather than `get`, because reading a secret can
    /// cost a per-entry unlock prompt. The default reads; a backend that can answer from
    /// metadata alone overrides it.
    fn exists(&self, key: &str) -> Result<bool, ConfigError> {
        self.get(key).map(|v| v.is_some())
    }
}

/// Test double; also handy for callers that hold a password only for one run.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn get(&self, key: &str) -> Result<Option<String>, ConfigError> {
        Ok(self.0.lock().expect("memory store").get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), ConfigError> {
        self.0
            .lock()
            .expect("memory store")
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), ConfigError> {
        self.0.lock().expect("memory store").remove(key);
        Ok(())
    }
}

/// Passwords as files: `<dir>/<key with ':' and '/' replaced by '_'>`, dir 0700, file 0600.
pub struct FileStore {
    dir: std::path::PathBuf,
}

impl FileStore {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> FileStore {
        FileStore { dir: dir.into() }
    }

    pub fn path_for(&self, key: &str) -> std::path::PathBuf {
        self.dir.join(key.replace([':', '/'], "_"))
    }
}

impl SecretStore for FileStore {
    fn name(&self) -> &'static str {
        "file"
    }

    fn get(&self, key: &str) -> Result<Option<String>, ConfigError> {
        let path = self.path_for(key);
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(s.trim_end_matches(['\n', '\r']).to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigError::Read { path, source: e }),
        }
    }

    /// Writes through a uniquely named 0600 temp file opened with `create_new` and renames it
    /// over the target, so a pre-existing file's mode is never inherited and a symlink planted
    /// at the target is replaced rather than followed. A directory this call creates is 0700;
    /// an existing one is left alone.
    fn set(&self, key: &str, value: &str) -> Result<(), ConfigError> {
        let path = self.path_for(key);
        crate::atomic::create_dir_private(&self.dir).map_err(|source| ConfigError::Write {
            path: self.dir.clone(),
            source,
        })?;
        // `path` is `self.dir.join(...)`, so this lands beside it in `self.dir`.
        let tmp = crate::atomic::temp_name(&path);
        let wrap = |source| ConfigError::Write {
            path: path.clone(),
            source,
        };
        let text = value.trim_end_matches(['\n', '\r']);
        if let Err(e) =
            crate::atomic::write_temp(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path))
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(wrap(e));
        }
        crate::atomic::sync_dir(&self.dir).map_err(wrap)
    }

    fn delete(&self, key: &str) -> Result<(), ConfigError> {
        let path = self.path_for(key);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ConfigError::Write { path, source: e }),
        }
    }
}

/// Ordered backends. Reads return the first hit; writes go to the first backend that accepts;
/// deletes hit every backend. An unavailable backend is skipped, and only reported when no
/// backend could serve the request.
pub struct Secrets {
    stores: Vec<Box<dyn SecretStore>>,
}

/// The platform keyring, unless `NUTSH_KEYRING=0` or the platform store cannot be created.
#[cfg(any(
    target_os = "macos",
    all(target_os = "linux", feature = "linux-keyring")
))]
fn keyring_store() -> Option<Box<dyn SecretStore>> {
    let enabled = std::env::var("NUTSH_KEYRING")
        .map(|v| v != "0")
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    crate::keyring_store::KeyringStore::new().map(|k| Box::new(k) as Box<dyn SecretStore>)
}

/// No keyring backend is compiled in on this target.
#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", feature = "linux-keyring")
)))]
fn keyring_store() -> Option<Box<dyn SecretStore>> {
    None
}

impl Secrets {
    pub fn with(stores: Vec<Box<dyn SecretStore>>) -> Secrets {
        Secrets { stores }
    }

    /// Keyring (when compiled in and `NUTSH_KEYRING` is not `0`), then the file store.
    pub fn default_stores() -> Secrets {
        let mut stores: Vec<Box<dyn SecretStore>> = Vec::new();
        stores.extend(keyring_store());
        stores.push(Box::new(FileStore::new(crate::paths::secrets_dir())));
        Secrets { stores }
    }

    /// An unavailable backend is only reported when no backend answered at all; a clean
    /// not-found from any backend counts as served.
    pub fn get(&self, key: &str) -> Result<Option<(&'static str, String)>, ConfigError> {
        let mut unavailable = None;
        let mut served = false;
        for s in &self.stores {
            match s.get(key) {
                Ok(Some(v)) => return Ok(Some((s.name(), v))),
                Ok(None) => served = true,
                Err(e) => {
                    unavailable.get_or_insert(e);
                }
            }
        }
        match unavailable {
            Some(e) if !served => Err(e),
            _ => Ok(None),
        }
    }

    /// Which backend holds this key, without reading the password out of it. An unavailable
    /// backend is skipped: not knowing is reported as "not here", never as an error, because
    /// this only ever labels a column.
    pub fn locate(&self, key: &str) -> Option<&'static str> {
        self.stores
            .iter()
            .find(|s| s.exists(key).unwrap_or(false))
            .map(|s| s.name())
    }

    pub fn set(&self, key: &str, value: &str) -> Result<&'static str, ConfigError> {
        let mut reasons = Vec::new();
        for s in &self.stores {
            match s.set(key, value) {
                Ok(()) => return Ok(s.name()),
                Err(e) => reasons.push(format!("{}: {e}", s.name())),
            }
        }
        Err(ConfigError::SecretUnavailable(reasons.join("; ")))
    }

    /// Deletes from every backend that works, and returns one warning per backend that did
    /// not (`"{name} unreachable: {e}"`) for the caller to print. `Err` only when no backend
    /// could be reached at all, since then nothing was deleted anywhere.
    pub fn delete(&self, key: &str) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();
        let mut first_err = None;
        let mut deleted = false;
        for s in &self.stores {
            match s.delete(key) {
                Ok(()) => deleted = true,
                Err(e) => {
                    warnings.push(format!("{} unreachable: {e}", s.name()));
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) if !deleted => Err(e),
            _ => Ok(warnings),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("secrets"));
        assert_eq!(store.get("admin@pc:9440").unwrap(), None);
        assert!(!store.exists("admin@pc:9440").unwrap());
        store.set("admin@pc:9440", "s3cret\n").unwrap();
        assert_eq!(
            store.get("admin@pc:9440").unwrap().as_deref(),
            Some("s3cret")
        );
        assert!(store.exists("admin@pc:9440").unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = dir.path().join("secrets").join("admin@pc_9440");
            assert_eq!(
                std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(dir.path().join("secrets"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        store.delete("admin@pc:9440").unwrap();
        store.delete("admin@pc:9440").unwrap();
        assert_eq!(store.get("admin@pc:9440").unwrap(), None);
    }

    struct Broken;
    impl SecretStore for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn get(&self, _: &str) -> Result<Option<String>, ConfigError> {
            Err(ConfigError::SecretUnavailable("no daemon".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), ConfigError> {
            Err(ConfigError::SecretUnavailable("no daemon".into()))
        }
        fn delete(&self, _: &str) -> Result<(), ConfigError> {
            Err(ConfigError::SecretUnavailable("no daemon".into()))
        }
    }

    #[test]
    fn facade_falls_back_past_an_unavailable_store() {
        let secrets = Secrets::with(vec![Box::new(Broken), Box::new(MemoryStore::default())]);
        assert_eq!(secrets.set("k", "v").unwrap(), "memory");
        assert_eq!(secrets.get("k").unwrap(), Some(("memory", "v".to_string())));
        assert_eq!(secrets.locate("k"), Some("memory"));
        let warnings = secrets.delete("k").unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with("broken unreachable"),
            "{:?}",
            warnings[0]
        );
        assert_eq!(secrets.get("k").unwrap(), None);
        let only_broken = Secrets::with(vec![Box::new(Broken)]);
        assert!(matches!(
            only_broken.set("k", "v"),
            Err(ConfigError::SecretUnavailable(_))
        ));
        assert!(matches!(
            only_broken.get("k"),
            Err(ConfigError::SecretUnavailable(_))
        ));
        assert!(matches!(
            only_broken.delete("k"),
            Err(ConfigError::SecretUnavailable(_))
        ));
    }

    /// A password file someone else planted must not keep its own mode: the rename swaps in
    /// our 0600 temp file, and no temp file is left behind.
    #[cfg(unix)]
    #[test]
    fn file_store_replaces_a_planted_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets");
        let store = FileStore::new(&secrets);
        let path = store.path_for("admin@pc:9440");
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.set("admin@pc:9440", "new").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(store.get("admin@pc:9440").unwrap().as_deref(), Some("new"));
        let left: Vec<_> = std::fs::read_dir(&secrets)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("admin@pc_9440")]);
    }

    /// A symlink planted at the password path must be replaced, not followed: writing through
    /// it would hand an attacker our password or let them clobber a file we can write.
    #[cfg(unix)]
    #[test]
    fn file_store_does_not_follow_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets");
        let store = FileStore::new(&secrets);
        std::fs::create_dir_all(&secrets).unwrap();
        let target = dir.path().join("victim");
        std::fs::write(&target, "untouched").unwrap();
        let link = store.path_for("admin@pc:9440");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        store.set("admin@pc:9440", "new").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_file()
        );
        assert_eq!(store.get("admin@pc:9440").unwrap().as_deref(), Some("new"));
    }
}
