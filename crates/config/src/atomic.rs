//! Writing a file the way this program writes every file it owns: a 0700 directory it creates
//! itself, a `create_new` temp file at 0600 so the mode is never a pre-planted file's,
//! `sync_all`, a rename, and a `sync_dir` so the rename itself survives a crash.
//!
//! `file.rs` has used these since the config file existed; `nutsh_core::cache` uses the same
//! three, so nothing about the write rule is restated in `core`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// `mkdir -p` where every directory *this call* creates is 0700. `DirBuilder`'s mode is
/// applied by `mkdir(2)` itself, so an existing directory is never chmodded and there is no
/// window between the test and the change.
pub fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Create `tmp` (which must not exist) with mode 0600 and flush its contents to disk.
pub fn write_temp(tmp: &Path, text: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

/// Persist the rename itself: without this the new name can be lost to a crash even though the
/// data reached the disk. Only unix lets us open a directory.
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// A unique temp name beside `path`, for the write-then-rename dance. Crate-private: every
/// caller outside `config` writes through [`write_private`], which does the whole dance.
pub(crate) fn temp_name(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

/// Write `text` to `path` atomically at 0600, creating the directory 0700 if it is missing.
/// The temp file is removed if anything fails, so a failed write leaves no litter.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_private(dir)?;
    let tmp = temp_name(path);
    if let Err(e) = write_temp(&tmp, text).and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three primitives `save` has always used, now public and used by the cache as well:
    /// a directory this call creates is 0700, a file it writes is 0600, and neither is ever a
    /// pre-planted file's mode.
    #[cfg(unix)]
    #[test]
    fn a_created_directory_is_private_and_a_written_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        create_dir_private(&nested).unwrap();
        assert_eq!(mode(&nested), 0o700);
        let tmp = nested.join(".x.tmp");
        write_temp(&tmp, "hello").unwrap();
        assert_eq!(mode(&tmp), 0o600);
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "hello");
        // `create_new`, so a planted file cannot survive with its own mode.
        assert_eq!(
            write_temp(&tmp, "again").unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        sync_dir(&nested).unwrap();
    }
}
