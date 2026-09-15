//! `⏎` on a refresh row: the whole ladder at once, with the rung in force marked.
//!
//! `space` steps one rung and shows one value; nothing on that row says what the other nine
//! are or which way the next press goes. The skin row opens a list for the same reason.

use nutsh_core::contexts::Interval;
use nutsh_core::contexts::Schedule;
use nutsh_core::refresh::{LADDER, show};

use crate::key::Key;
use crate::palette::window;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Moved,
    Chose(Interval),
    Cancelled,
    Ignored,
}

#[derive(Debug)]
pub struct Ladder {
    /// Which level of `[refresh]` the choice is written to.
    pub at: Schedule<'static>,
    /// What the box is titled after: a kind's display name, a namespace, or `every kind`.
    pub label: String,
    /// The rung in force when the list opened, marked in the list.
    pub current: Option<Interval>,
    pub selected: usize,
}

impl Ladder {
    /// Opens on the rung in force, so `esc` and `⏎` on the same row both change nothing.
    pub fn open(at: Schedule<'static>, label: String, current: Option<Interval>) -> Ladder {
        Ladder {
            at,
            label,
            current,
            selected: current
                .and_then(|v| LADDER.iter().position(|r| *r == v))
                .unwrap_or(0),
        }
    }

    pub fn key(&mut self, key: Key) -> Action {
        let last = LADDER.len().saturating_sub(1);
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
            Key::Char('g') | Key::Home => {
                self.selected = 0;
                Action::Moved
            }
            Key::Char('G') | Key::End => {
                self.selected = last;
                Action::Moved
            }
            Key::Enter | Key::Char(' ') => Action::Chose(LADDER[self.selected.min(last)]),
            _ => Action::Ignored,
        }
    }

    /// The rungs, spelled the way the settings row spells them.
    pub fn rows(&self) -> Vec<(String, bool)> {
        LADDER
            .iter()
            .map(|r| (show(*r), Some(*r) == self.current))
            .collect()
    }

    pub fn window(&self, rows: usize) -> (usize, &'static [Interval]) {
        window(LADDER, self.selected, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list opens on the rung in force and `⏎` names the one under the cursor.
    #[test]
    fn opens_on_the_rung_in_force_and_names_the_choice() {
        let mut l = Ladder::open(
            Schedule::Kind("vmm.ahv.config.Vm"),
            "Virtual Machines".into(),
            Some(Interval::Secs(10)),
        );
        assert_eq!(l.selected, 2);
        assert_eq!(l.key(Key::Enter), Action::Chose(Interval::Secs(10)));
        assert_eq!(l.key(Key::Char('j')), Action::Moved);
        assert_eq!(l.key(Key::Enter), Action::Chose(Interval::Secs(30)));
        assert_eq!(l.key(Key::Char('G')), Action::Moved);
        assert_eq!(l.key(Key::Enter), Action::Chose(*LADDER.last().unwrap()));
        assert_eq!(l.key(Key::Esc), Action::Cancelled);
    }

    /// A value off the ladder - `:refresh 45` - opens at the top rather than nowhere.
    #[test]
    fn a_value_off_the_ladder_opens_at_the_top() {
        let l = Ladder::open(
            Schedule::Everything,
            "every kind".into(),
            Some(Interval::Secs(45)),
        );
        assert_eq!(l.selected, 0);
        assert!(l.rows().iter().all(|(_, current)| !current));
    }
}
