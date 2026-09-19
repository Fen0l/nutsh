//! `:export [csv|json] [path]`: the table in view, written as it is drawn.

use super::*;
use nutsh_core::export::{Format, Sheet};

impl App {
    /// The rows of the table view as a sheet: the columns the view shows, in its sort, under
    /// its `/` filter, with the names the table resolved. `None` off a table view.
    pub fn sheet(&self) -> Option<Sheet> {
        let live = self.live.as_ref()?;
        let view = live.table()?;
        let cols = crate::table::columns_for(view.key.kind, view.wide, self.merges(&view.key));
        let table = live.merged(&view.key);
        let rows = crate::table::cells(
            &table,
            &cols,
            live.store.names(),
            self.now,
            view.sort,
            view.query(),
        );
        Some(Sheet {
            headers: cols.iter().map(|c| c.header.to_string()).collect(),
            rows: rows
                .into_iter()
                .map(|(_, cells)| cells.into_iter().map(|c| c.text).collect())
                .collect(),
        })
    }

    /// The table in view rendered as `format`, for `--snapshot --format`. Off a table view it
    /// is an error, not an empty file: a page has several tables and a script asked for one.
    pub fn export(&self, format: Format) -> anyhow::Result<String> {
        let sheet = self
            .sheet()
            .ok_or_else(|| anyhow::anyhow!("export: not on a table; open a kind first"))?;
        Ok(sheet.render(format))
    }

    /// `:export [csv|json] [path]`. CSV when no format is named; a file in the working
    /// directory stamped to the second when no path is.
    pub(super) fn export_command(&mut self, args: &[String]) {
        let mut format = Format::Csv;
        let mut path: Option<&str> = None;
        for word in args {
            match Format::parse(word) {
                Ok(f) if path.is_none() => format = f,
                // A bare word that is not a format is a typo, not a file called `xlsx`.
                Err(why) if path.is_none() && !word.contains(['/', '.']) => {
                    self.status = Some(format!("export: {why}"));
                    return;
                }
                _ if path.is_none() => path = Some(word),
                _ => {
                    self.status = Some("export: [csv|json] [path], nothing more".into());
                    return;
                }
            }
        }
        let Some(sheet) = self.sheet() else {
            self.status = Some("export: not on a table; open a kind first".into());
            return;
        };
        let kind = self
            .current_kind()
            .and_then(|k| k.aliases.first().copied())
            .unwrap_or("table");
        let path = path.map_or_else(
            || nutsh_core::export::default_path(kind, format, self.now),
            str::to_string,
        );
        let n = sheet.rows.len();
        self.status = Some(match write_private(&path, &sheet.render(format)) {
            Ok(()) => format!(
                "exported {n} row{} to {path}",
                if n == 1 { "" } else { "s" }
            ),
            Err(e) => format!("export: {path}: {e}"),
        });
        self.dirty = true;
    }
}

/// An export is an inventory: written 0600 where the platform can say so, the way the config
/// file and the cache are, and never through a symlink somebody left at the name.
fn write_private(path: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other("refusing to write through a symlink"));
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(text.as_bytes())
}
