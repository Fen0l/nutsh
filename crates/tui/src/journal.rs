//! `:journal`: what was attempted this session. Newest first, `esc` closes.

use crate::key::Key;

/// What a key did, for the app to act on, named the way every other modal in this crate names
/// it: the view cannot reach the mode it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Moved,
    Closed,
    Ignored,
}

#[derive(Debug, Default)]
pub struct JournalView {
    pub selected: usize,
}

impl JournalView {
    /// `last` is the index of the last row; the app measures it.
    pub fn key(&mut self, key: Key, last: usize) -> Action {
        match key {
            Key::Esc | Key::Char('q') => return Action::Closed,
            Key::Char('j') | Key::Down => self.selected = (self.selected + 1).min(last),
            Key::Char('k') | Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Char('g') | Key::Home => self.selected = 0,
            Key::Char('G') | Key::End => self.selected = last,
            Key::PageDown => self.selected = (self.selected + 20).min(last),
            Key::PageUp => self.selected = self.selected.saturating_sub(20),
            _ => return Action::Ignored,
        }
        Action::Moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window is the app's, so the view's whole job is a bounded index: `last` is measured
    /// per keypress and an empty ring is `last = 0`, which every arm has to survive.
    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut v = JournalView::default();
        assert_eq!(
            v.key(Key::Char('k'), 3),
            Action::Moved,
            "nothing above the first row"
        );
        assert_eq!(v.selected, 0);
        v.key(Key::Char('G'), 3);
        assert_eq!(v.selected, 3);
        v.key(Key::Down, 3);
        assert_eq!(v.selected, 3, "and nothing below the last");
        v.key(Key::PageUp, 3);
        assert_eq!(v.selected, 0);
        v.key(Key::PageDown, 3);
        assert_eq!(v.selected, 3);
        v.key(Key::Char('g'), 3);
        assert_eq!(v.selected, 0);
        // An empty ring: every movement key still has an index to land on.
        let mut empty = JournalView::default();
        for key in [Key::Char('j'), Key::Char('G'), Key::PageDown, Key::End] {
            assert_eq!(empty.key(key, 0), Action::Moved);
            assert_eq!(empty.selected, 0);
        }
        assert_eq!(
            empty.key(Key::Char('z'), 0),
            Action::Ignored,
            "and nothing else moves it"
        );
        assert_eq!(empty.key(Key::Esc, 0), Action::Closed, "esc closes");
        assert_eq!(
            empty.key(Key::Char('q'), 0),
            Action::Closed,
            "and so does q"
        );
    }
}
