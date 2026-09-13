//! `:skin` with no argument: the built-in skins, with the one showing selected.

use crate::key::Key;
use crate::palette::window;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Moved,
    Chose(&'static str),
    Cancelled,
    Ignored,
}

#[derive(Debug)]
pub struct Skins {
    pub selected: usize,
}

impl Skins {
    /// Opens on the skin now showing, so `esc` and `enter` on the same row both change nothing.
    pub fn open() -> Skins {
        let current = crate::theme::current_name();
        Skins {
            selected: crate::theme::BUILTIN_NAMES
                .iter()
                .position(|n| *n == current)
                .unwrap_or(0),
        }
    }

    pub fn key(&mut self, key: Key) -> Action {
        let last = crate::theme::BUILTIN_NAMES.len().saturating_sub(1);
        match key {
            Key::Esc => Action::Cancelled,
            Key::Down | Key::Tab | Key::Char('j') => {
                self.selected = (self.selected + 1).min(last);
                Action::Moved
            }
            Key::Up | Key::BackTab | Key::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::Moved
            }
            Key::Enter => Action::Chose(crate::theme::BUILTIN_NAMES[self.selected.min(last)]),
            _ => Action::Ignored,
        }
    }

    pub fn window(&self, rows: usize) -> (usize, &'static [&'static str]) {
        window(crate::theme::BUILTIN_NAMES, self.selected, rows)
    }
}
