//! The palette's line history: per context, beside the cache, under the same switch.
//!
//! It lives here rather than in `nutsh-tui` because it needs `nutsh_config::atomic` and the
//! cache directory, both of which `core` owns, and because the TUI does no file IO at all.
//!
//! **One switch governs the history and the cache.** `cache = false`, `--no-cache`, `--check`,
//! a slug that is not a usable directory name, or a format this build does not know all mean
//! the history is never read and never written. A user who turned the cache off did so to stop
//! this machine remembering which Prism Central they touched, and the typed line `:ctx prod-eu`
//! is that same fact.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The on-disk format. A file this build does not know is not read, not written and not
/// deleted.
pub const FORMAT: u32 = 1;
/// Newest last. A screenful is twelve rows; a hundred covers a long session and costs about
/// 2 KB.
pub const MAX_ENTRIES: usize = 100;
/// The longest kind id is under 45 characters and no legitimate palette line is anywhere near
/// this, so one that is came from a paste. Belt and braces beside `is_key_material`, which
/// already refuses anything over 100 bytes: this rule survives that one changing.
pub const MAX_LINE: usize = 120;

/// The file's name. Crate-visible because `cache::read` has to know the one file in that
/// directory that is not the cache's to throw away.
pub(crate) const FILE: &str = "history.json";

/// `{ "version": 1, "entries": ["vm", "ctx lab", "skin nord"] }` - newest **last**, so the file
/// reads chronologically.
#[derive(Debug, Serialize, Deserialize)]
struct File {
    version: u32,
    entries: Vec<String>,
}

#[derive(Debug, Default)]
pub struct History {
    /// Where to write. `None` is a session-only history: this run does not cache, or the file
    /// on disk is a format this build must not touch.
    path: Option<PathBuf>,
    entries: Vec<String>,
}

impl History {
    /// The history in `dir`, or a session-only one when there is no directory.
    pub fn load(dir: Option<&Path>) -> History {
        let Some(dir) = dir else {
            return History::default();
        };
        let path = dir.join(FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            // No file yet is not a refusal: the first accepted line creates it.
            return History {
                path: Some(path),
                entries: Vec::new(),
            };
        };
        match serde_json::from_str::<File>(&text) {
            Ok(file) if file.version == FORMAT => History {
                path: Some(path),
                entries: file.entries,
            },
            // A newer or unreadable file: read nothing, write nothing, delete nothing.
            _ => History::default(),
        }
    }

    /// Newest last.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Record an accepted line and write the file.
    ///
    /// On each accepted entry rather than on a debounce: one atomic 2 KB write per `enter` in
    /// the palette is not a performance question, and a debounce would drop the last line at
    /// quit. A failed write is silent - a history that could not be saved must not take the
    /// palette's `enter` with it - and `nutsh cache info` is where a broken directory shows.
    pub fn push(&mut self, line: &str) {
        let line = line.trim();
        if !worth_keeping(line) {
            return;
        }
        // Move-to-end rather than repeat, so `ctrl-p` twice reaches the second-most-recent
        // *distinct* line: `HISTCONTROL=erasedups`.
        self.entries.retain(|e| e != line);
        self.entries.push(line.to_string());
        let over = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(..over);
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let file = File {
            version: FORMAT,
            entries: self.entries.clone(),
        };
        if let Ok(text) = serde_json::to_string(&file) {
            let _ = nutsh_config::atomic::write_private(path, &text);
        }
    }
}

/// Never an empty line, never one long enough to be a paste, and never anything shaped like key
/// material: the palette has no password field, but `:ctx` takes free text and a password
/// pasted into the wrong window must not become a file.
fn worth_keeping(line: &str) -> bool {
    !line.is_empty() && line.len() <= MAX_LINE && !nutsh_catalog::secret::is_key_material(line)
}

/// How many lines one context's history holds, for `nutsh cache info`. `None` when there is no
/// history file, or when it is a format this build does not read.
pub fn count(dir: &Path) -> Option<usize> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    let file: File = serde_json::from_str(&text).ok()?;
    (file.version == FORMAT).then_some(file.entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(h: &History) -> Vec<&str> {
        h.entries().iter().map(String::as_str).collect()
    }

    /// Newest last, so the file reads chronologically and `ctrl-p` walks backwards from the
    /// end. A repeat moves to the end rather than being repeated - `HISTCONTROL=erasedups` -
    /// so `ctrl-p` twice reaches the second-most-recent *distinct* line.
    #[test]
    fn a_repeat_moves_to_the_end_and_the_cap_trims_the_front() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lab");
        let mut h = History::load(Some(&dir));
        assert!(entries(&h).is_empty(), "no file yet is not a refusal");
        for line in ["vm", "ctx lab", "vm"] {
            h.push(line);
        }
        assert_eq!(
            entries(&h),
            ["ctx lab", "vm"],
            "moved to the end, not repeated"
        );

        // It survives the process: the same directory reads back what was written.
        let h = History::load(Some(&dir));
        assert_eq!(entries(&h), ["ctx lab", "vm"]);

        // And it is capped from the front.
        let mut h = History::load(Some(&dir));
        for i in 0..MAX_ENTRIES + 10 {
            h.push(&format!("kind-{i}"));
        }
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        assert_eq!(
            h.entries().last().unwrap(),
            &format!("kind-{}", MAX_ENTRIES + 9)
        );
        assert_eq!(h.entries().first().unwrap(), "kind-10");
    }

    /// `:ctx` takes free text and a password pasted into the wrong window must not become a
    /// file.
    #[test]
    fn nothing_worth_hiding_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lab");
        let mut h = History::load(Some(&dir));
        h.push("   ");
        h.push("");
        h.push("ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAB");
        h.push("-----BEGIN OPENSSH PRIVATE KEY-----");
        h.push(&"x".repeat(MAX_LINE + 1));
        assert!(entries(&h).is_empty(), "{:?}", h.entries());
        // Trimmed, and then kept.
        h.push("  ctx lab  ");
        assert_eq!(entries(&h), ["ctx lab"]);
    }

    /// The cache's rule for a newer on-disk format, applied unchanged: do not read, do not
    /// write, do not delete.
    #[test]
    fn a_newer_format_is_not_read_not_written_and_not_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lab");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        std::fs::write(&path, r#"{"version":99,"entries":["from the future"]}"#).unwrap();
        let mut h = History::load(Some(&dir));
        assert!(entries(&h).is_empty(), "not read");
        h.push("vm");
        assert_eq!(entries(&h), ["vm"], "the session still has one");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"version":99,"entries":["from the future"]}"#,
            "not written and not deleted"
        );
        assert_eq!(count(&dir), None, "and `cache info` does not count it");
    }

    /// The switch: no directory means a session-only history and no file anywhere.
    #[test]
    fn no_history_file_without_a_cache_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = History::load(None);
        h.push("vm");
        h.push("ctx lab");
        assert_eq!(
            entries(&h),
            ["vm", "ctx lab"],
            "a session-only history still works"
        );
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "and wrote nothing"
        );
    }

    /// 0600 inside the 0700 directory `write_private` creates, like every file this program
    /// owns.
    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lab");
        let mut h = History::load(Some(&dir));
        h.push("vm");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("history.json")), 0o600);
        assert_eq!(count(&dir), Some(1));
    }
}
