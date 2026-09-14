//! `:settings`: what is set, what it is set to, who set it - and the `[nav] hide` list.
//!
//! A setting the screen cannot honestly change is drawn read-only with its reason, never left
//! out: a setting that is on and not listed is one the user will hunt for in the wrong file.

use nutsh_core::contexts::{Setting, SettingId};

use crate::key::Key;
use crate::palette::window;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// One namespace of the refresh section: its own schedule, and the kinds under it when it
    /// is open. Twenty of these beat two hundred and thirty-two rows of which most are the same.
    Namespace {
        name: &'static str,
        value: String,
        source: String,
        open: bool,
        /// How many of its kinds carry a schedule of their own.
        custom: usize,
    },
    /// A dim rule with a word on it; never selected, always skipped.
    Heading(&'static str),
    Setting {
        id: SettingId,
        /// Owned, because the per-kind refresh row's label carries the kind's display name and
        /// no `&'static str` can.
        label: String,
        value: String,
        source: String,
        /// Why this row can only be read, when it can only be read.
        fixed: Option<String>,
        /// Which kind this row schedules. `None` for every row that is not one of the per-kind
        /// refresh rows, including `[refresh] default`.
        kind: Option<&'static str>,
        /// How long ago this kind's table last completed a cycle, already rendered. `None` for
        /// a kind no table is open on, which has not refreshed rather than refreshed long ago.
        age: Option<String>,
    },
    /// One `[nav] hide` entry, as written.
    Hidden { name: String, source: &'static str },
    /// The hide list is empty: one line saying so beats a heading over nothing.
    Note(&'static str),
    /// An action rather than a setting: `enter` runs it, there is nothing to show.
    Run { label: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Moved,
    /// `space`/`enter` on a switch this screen writes itself.
    Toggled(SettingId, bool),
    /// `enter` on the skin row: the skin list opens over this one.
    Skins,
    /// `space` on a refresh row: seven values, not two, so it steps a ladder rather than
    /// flipping a bit. The app writes the new rung to the file. The kind is the row's own, not
    /// whatever view the screen was opened over.
    Cycled(SettingId, Option<&'static str>),
    /// `space` on a namespace row: the same ladder, written to `[refresh.namespaces]`.
    CycledNamespace(&'static str),
    /// `enter` on a namespace row: show or hide its kinds.
    Expand(&'static str),
    /// `enter` on the refresh-all row: every subscription cycles now.
    RefreshAll,
    /// A setting whose own palette command already owns the writer and the live effect -
    /// `mouse`, `header` and `log`. The screen asks for the command rather than becoming a
    /// second writer for the same key.
    Cycle(SettingId),
    /// `a`: the palette opens on `hide `, whose completion already knows every kind id, page id
    /// and group name.
    Add,
    /// `d`: this `[nav] hide` entry goes - the same path `:show` takes.
    Remove(String),
    /// A row this screen can only show: its reason, for the status line.
    Fixed(String),
    Cancelled,
    Ignored,
}

/// One schedule the screen can set: the setting itself, the label that names what it schedules,
/// the kind it belongs to, and how long ago that kind last polled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshRow {
    pub setting: Setting,
    pub label: String,
    pub kind: Option<&'static str>,
    pub age: Option<String>,
}

#[derive(Debug)]
pub struct Settings {
    pub rows: Vec<Row>,
    pub selected: usize,
}

impl Settings {
    /// `settings` is the seam's answer, `skin` the skin actually on screen - the file's value is
    /// stale for the rest of a `:skin` session, and the row is about what the user is looking
    /// at. `refresh` is the schedule of the view in front of the user, which lives in `App` and
    /// not behind the `Contexts` seam, and is a separate argument for the same reason `skin` is.
    /// `hide` is `[nav] hide` as the app holds it.
    pub fn open(settings: &[Setting], skin: &str, refresh: &[Row], hide: &[String]) -> Settings {
        let mut rows: Vec<Row> = settings
            .iter()
            .map(|s| Row::Setting {
                id: s.id,
                kind: None,
                age: None,
                label: s.id.label().to_string(),
                value: if s.id == SettingId::Skin {
                    skin.to_string()
                } else {
                    s.value.clone()
                },
                source: s.source.label(),
                fixed: s.fixed.clone(),
            })
            .collect();
        if rows.is_empty() {
            rows.push(Row::Note("no config file: nothing to change here"));
            return Settings { rows, selected: 0 };
        }
        rows.push(Row::Heading("HIDDEN"));
        if hide.is_empty() {
            rows.push(Row::Note("nothing hidden - a:add"));
        } else {
            rows.extend(hide.iter().map(|name| Row::Hidden {
                name: name.clone(),
                source: "config file",
            }));
        }
        // Last, and not between the switches and the hide list: there is one row per kind, and
        // a section that long in the middle puts everything after it past the end of the screen.
        if !refresh.is_empty() {
            rows.push(Row::Heading("REFRESH"));
            rows.push(Row::Run {
                label: "refresh everything now",
            });
            rows.extend(refresh.iter().cloned());
        }
        Settings { rows, selected: 0 }
    }

    pub fn key(&mut self, key: Key) -> Action {
        match key {
            Key::Esc => Action::Cancelled,
            Key::Down | Key::Tab | Key::Char('j') => self.step(1),
            Key::Up | Key::BackTab | Key::Char('k') => self.step(-1),
            Key::Char('g') | Key::Home => {
                self.selected = 0;
                self.skip(1);
                Action::Moved
            }
            Key::Char('G') | Key::End => {
                self.selected = self.rows.len().saturating_sub(1);
                self.skip(-1);
                Action::Moved
            }
            Key::Char('a') => Action::Add,
            Key::Char('d') => match self.rows.get(self.selected) {
                Some(Row::Hidden { name, .. }) => Action::Remove(name.clone()),
                _ => Action::Ignored,
            },
            Key::Char(' ') | Key::Enter => self.act(key),
            _ => Action::Ignored,
        }
    }

    /// `space` and `enter` do the row's own thing: a switch flips, the skin row opens the list a
    /// skin is picked from, `mouse`, `header` and `log` ask for the commands that own them, and
    /// a fixed row explains itself instead of lying about writing.
    fn act(&mut self, key: Key) -> Action {
        match self.rows.get(self.selected) {
            Some(Row::Setting {
                fixed: Some(why), ..
            }) => Action::Fixed(why.clone()),
            Some(Row::Run { .. }) => Action::RefreshAll,
            Some(Row::Namespace { name, .. }) => match key {
                Key::Enter => Action::Expand(name),
                _ => Action::CycledNamespace(name),
            },
            Some(Row::Setting {
                id: SettingId::Skin,
                ..
            }) => Action::Skins,
            Some(Row::Setting {
                id: id @ (SettingId::Mouse | SettingId::Header | SettingId::Log),
                ..
            }) => Action::Cycle(*id),
            // Seven values, not two: `space` steps a ladder here rather than flipping a bit.
            Some(Row::Setting {
                id: id @ (SettingId::Refresh | SettingId::RefreshKind),
                kind,
                ..
            }) => Action::Cycled(*id, *kind),
            Some(Row::Setting { id, value, .. }) => Action::Toggled(*id, value != "on"),
            _ => Action::Ignored,
        }
    }

    /// One row, then past any heading in that direction: a rule with a word on it is not
    /// something a cursor should be able to sit on.
    fn step(&mut self, by: isize) -> Action {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
        self.skip(by);
        Action::Moved
    }

    fn skip(&mut self, by: isize) {
        let last = self.rows.len().saturating_sub(1);
        while matches!(self.rows.get(self.selected), Some(Row::Heading(_))) {
            let next = self.selected.saturating_add_signed(by);
            if next > last {
                // At the end there is nothing past the heading; back up instead of stalling.
                self.selected = self.selected.saturating_sub(1);
                return;
            }
            self.selected = next;
        }
    }

    pub fn window(&self, rows: usize) -> (usize, &[Row]) {
        window(&self.rows, self.selected, rows)
    }
}

#[cfg(test)]
mod tests {
    use nutsh_core::contexts::Source;

    use super::*;

    fn setting(id: SettingId, value: &str, fixed: Option<&str>) -> Setting {
        Setting {
            id,
            value: value.to_string(),
            source: Source::Default,
            fixed: fixed.map(str::to_string),
        }
    }

    fn screen() -> Settings {
        Settings::open(
            &[
                setting(SettingId::Skin, "catppuccin-mocha", None),
                setting(SettingId::Cache, "on", None),
                setting(SettingId::Mouse, "on", None),
                setting(SettingId::Readonly, "off", Some("compiled at connect")),
                setting(SettingId::HideUnserved, "off", None),
            ],
            "gruvbox-dark",
            &[],
            &["Data Protection".to_string()],
        )
    }

    #[test]
    fn the_skin_row_shows_the_skin_on_screen_not_the_one_in_the_file() {
        let s = screen();
        assert!(matches!(
            &s.rows[0],
            Row::Setting { value, .. } if value == "gruvbox-dark"
        ));
    }

    #[test]
    fn the_cursor_never_lands_on_the_heading() {
        let mut s = screen();
        for _ in 0..10 {
            s.key(Key::Char('j'));
            assert!(!matches!(s.rows[s.selected], Row::Heading(_)));
        }
        s.key(Key::Char('G'));
        assert!(!matches!(s.rows[s.selected], Row::Heading(_)));
        for _ in 0..10 {
            s.key(Key::Char('k'));
            assert!(!matches!(s.rows[s.selected], Row::Heading(_)));
        }
    }

    #[test]
    fn a_fixed_row_explains_itself_and_toggles_nothing() {
        let mut s = screen();
        s.selected = 3;
        assert_eq!(
            s.key(Key::Char(' ')),
            Action::Fixed("compiled at connect".to_string())
        );
    }

    #[test]
    fn the_skin_row_opens_the_skin_list_and_mouse_asks_for_its_own_command() {
        let mut s = screen();
        s.selected = 0;
        assert_eq!(s.key(Key::Enter), Action::Skins);
        s.selected = 2;
        assert_eq!(s.key(Key::Char(' ')), Action::Cycle(SettingId::Mouse));
        s.selected = 1;
        assert_eq!(
            s.key(Key::Char(' ')),
            Action::Toggled(SettingId::Cache, false)
        );
    }

    #[test]
    fn d_removes_a_hide_entry_and_does_nothing_on_a_setting() {
        let mut s = screen();
        s.selected = 0;
        assert_eq!(s.key(Key::Char('d')), Action::Ignored);
        s.key(Key::Char('G'));
        assert_eq!(
            s.key(Key::Char('d')),
            Action::Remove("Data Protection".to_string())
        );
    }

    /// A seam with no config file behind it draws one line saying so, and no key writes.
    #[test]
    fn an_empty_seam_says_so_and_toggles_nothing() {
        let mut s = Settings::open(&[], "catppuccin-mocha", &[], &[]);
        assert_eq!(s.rows.len(), 1);
        assert!(matches!(s.rows[0], Row::Note(_)));
        for key in [Key::Char(' '), Key::Enter, Key::Char('d')] {
            assert_eq!(s.key(key), Action::Ignored);
        }
    }
}
