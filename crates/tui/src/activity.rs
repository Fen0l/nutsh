//! `:activity`: what this session is doing to the Prism Central. Two tabs over one question -
//! every request, newest first, and every table the session holds with when it last polled.

use crate::key::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Requests,
    Tables,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Moved,
    Closed,
    Ignored,
}

#[derive(Debug)]
pub struct ActivityView {
    pub tab: Tab,
    pub selected: usize,
}

impl Default for ActivityView {
    fn default() -> ActivityView {
        ActivityView {
            tab: Tab::Requests,
            selected: 0,
        }
    }
}

impl ActivityView {
    /// `last` is the index of the last row of the tab showing; the app measures it.
    pub fn key(&mut self, key: Key, last: usize) -> Action {
        match key {
            Key::Esc | Key::Char('q') => return Action::Closed,
            Key::Tab | Key::BackTab => {
                self.tab = match self.tab {
                    Tab::Requests => Tab::Tables,
                    Tab::Tables => Tab::Requests,
                };
                self.selected = 0;
            }
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

    /// `tab` flips the half and puts the cursor at the top; the cursor stops at both ends.
    #[test]
    fn tab_flips_the_half_and_the_cursor_stops_at_both_ends() {
        let mut v = ActivityView::default();
        assert_eq!(v.tab, Tab::Requests);
        v.key(Key::Char('G'), 7);
        assert_eq!(v.selected, 7);
        assert_eq!(v.key(Key::Tab, 7), Action::Moved);
        assert_eq!((v.tab, v.selected), (Tab::Tables, 0));
        v.key(Key::Char('j'), 0);
        assert_eq!(v.selected, 0, "one row: nothing below it");
        assert_eq!(v.key(Key::BackTab, 0), Action::Moved);
        assert_eq!(v.tab, Tab::Requests);
        assert_eq!(v.key(Key::Char('z'), 0), Action::Ignored);
        assert_eq!(v.key(Key::Esc, 0), Action::Closed);
        assert_eq!(v.key(Key::Char('q'), 0), Action::Closed);
    }
}
