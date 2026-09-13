//! One line of typed text with a cursor in it.
//!
//! The palette, the Contexts add form, the login field and an action's form all hold text the
//! user edits rather than a filter they type and clear, and all four were append-only: a typo in
//! the fourth character of a hostname cost the whole rest of the field. They share this.
//!
//! The three *filters* - `menu::Menu`, `picker::Picker` and `form::RefPicker` - keep a plain
//! `String` on purpose (the design's §2 and §8.5): nobody edits the middle of a filter, and the
//! two modules that hold them are under other plans' hands.
//!
//! The cursor is a **byte** index and is always on a char boundary, so slicing on it never
//! panics; it is also always on a **cluster** boundary, so a cursor can never stand inside what
//! the terminal draws as one cell. Every mover returns whether it moved, so a widget can answer
//! `Ignored` at an edge - and [`Input::key`] is the one implementation of the eight editing keys
//! that all four widgets bind, so there is one place a rule about them can be written.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::key::Key;

/// What [`Input::key`] did with a key it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// The text changed: whatever ranks or validates it has to run again.
    Changed,
    /// Only the cursor moved.
    Moved,
    /// This type owns the key, but there was nowhere to go - a `←` at the start of the line.
    Ignored,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Input {
    text: String,
    /// A byte index into `text`, always on a char boundary.
    cursor: usize,
}

/// The zero-width joiner. It binds the character after it into the cluster before it, which is
/// how a family emoji is one cell rather than three.
const ZWJ: char = '\u{200D}';

/// The byte index just past the cluster that starts at `at`.
///
/// A cluster here is the character at `at` plus every following character that takes no cell of
/// its own: a combining mark, a variation selector, a zero-width joiner, and the character a
/// joiner binds to it. It is deliberately **not** a full Unicode grapheme segmenter - that would
/// be a new dependency and this phase adds none - but it covers what a terminal actually draws as
/// one cell, which is what the cursor and `ui::cursor_spans` both have to agree about: `e` plus
/// U+0301, `👍` plus U+FE0F, and a ZWJ sequence.
///
/// Read by [`Input::left`], [`Input::right`] and by the drawing, so the cursor's idea of one cell
/// and the frame's cannot drift.
pub fn cluster_end(text: &str, at: usize) -> usize {
    let mut chars = text[at..].char_indices();
    let Some((_, first)) = chars.next() else {
        return at;
    };
    let mut end = at + first.len_utf8();
    let mut joined = first == ZWJ;
    for (i, c) in chars {
        if !joined && c.width().unwrap_or(0) != 0 {
            break;
        }
        end = at + i + c.len_utf8();
        joined = c == ZWJ;
    }
    end
}

/// The start of the cluster that ends at or before `at`: where `←` from `at` lands.
///
/// Walked from the start of the line rather than backwards, because the rule reads forwards and a
/// backwards approximation of it is a second rule. A palette line is one line, so this is a few
/// dozen characters per keystroke.
fn cluster_start(text: &str, at: usize) -> usize {
    let mut start = 0;
    while start < at {
        let end = cluster_end(text, start);
        if end >= at {
            return start;
        }
        start = end;
    }
    start
}

/// A cursor move that hit an edge is a key that did nothing, and says so.
fn moved(did: bool) -> Edit {
    if did { Edit::Moved } else { Edit::Ignored }
}

impl Input {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The byte index the next character lands at. Always a char boundary, so `&text[..cursor]`
    /// and `&text[cursor..]` are both safe.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether the cursor is past the last character - which is where the ghost is allowed to
    /// exist, so the two can never fight (design §8.3).
    pub fn at_end(&self) -> bool {
        self.cursor == self.text.len()
    }

    /// Replace the whole line and put the cursor at its end: what history recall does.
    pub fn set(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert a whole run at the cursor: what accepting a ghost does.
    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Delete the character before the cursor. `false` when there is none, so a widget can
    /// answer `Ignored` rather than reporting an edit that did not happen.
    pub fn backspace(&mut self) -> bool {
        let Some(prev) = self.text[..self.cursor].chars().next_back() else {
            return false;
        };
        let start = self.cursor - prev.len_utf8();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    /// `ctrl-w`, readline's: skip the spaces left of the cursor, then delete the run of
    /// non-spaces. Only `' '` separates - `Key::Tab` is a key in this app and never a character,
    /// so a tab cannot be in the line to begin with.
    pub fn delete_word(&mut self) -> bool {
        let head = &self.text[..self.cursor];
        let trimmed = head.trim_end_matches(' ');
        let start = trimmed.rfind(' ').map_or(0, |i| i + 1);
        if start == self.cursor {
            return false;
        }
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    /// `ctrl-u`: everything from the cursor back to the start, and nothing after it.
    pub fn delete_to_start(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.text.replace_range(..self.cursor, "");
        self.cursor = 0;
        true
    }

    /// One cell to the left. Never into the middle of a cluster: `e´` is two scalars, one cell
    /// and one step.
    pub fn left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor = cluster_start(&self.text, self.cursor);
        true
    }

    /// One cell to the right, on the same rule.
    pub fn right(&mut self) -> bool {
        if self.at_end() {
            return false;
        }
        self.cursor = cluster_end(&self.text, self.cursor);
        true
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Where to start drawing so the cursor is inside `room` cells, and whether anything was
    /// dropped from the left - which the caller draws as a leading `…`.
    ///
    /// A guard, not a feature (design §8.4, §15): the longest kind id is under 45 cells and the
    /// prompt line is the whole frame width, so on every real input this answers `(false, 0)`.
    /// Only the left needs a window, because ratatui's `Paragraph` clips the right edge itself.
    pub fn window(&self, room: usize) -> (bool, usize) {
        if self.text[..self.cursor].width() < room {
            return (false, 0);
        }
        let mut start = self.cursor;
        // One cell for the cursor itself; one more for the `…` this is about to earn.
        let mut cells = 1;
        for (i, c) in self.text[..self.cursor].char_indices().rev() {
            let w = c.width().unwrap_or(0);
            if cells + w + 1 > room {
                break;
            }
            cells += w;
            start = i;
        }
        (true, start)
    }

    /// The eight editing keys every text widget binds, in one implementation.
    ///
    /// `None` is "not mine": the widget answers for it. `enter`, `esc`, `tab`, the arrows that
    /// move a list and `ctrl-p`/`ctrl-n` all come back `None`, which is why the palette can bind
    /// `→` to a ghost and the action form can bind `←`/`→` to an enum without either of them
    /// re-implementing `ctrl-w`.
    ///
    /// A deletion with nothing to delete is still `Changed`, exactly as `String::pop` on an empty
    /// input always was: the widget re-ranks, finds the same rows and draws the same frame, and a
    /// `bool` here would make `backspace` on an empty line the one key that does not re-rank.
    pub fn key(&mut self, key: Key) -> Option<Edit> {
        let edit = match key {
            Key::Char(c) => {
                self.insert(c);
                Edit::Changed
            }
            Key::Backspace => {
                self.backspace();
                Edit::Changed
            }
            Key::Ctrl('w') => {
                self.delete_word();
                Edit::Changed
            }
            Key::Ctrl('u') => {
                self.delete_to_start();
                Edit::Changed
            }
            Key::Left => moved(self.left()),
            Key::Right => moved(self.right()),
            // `ctrl-a` and `ctrl-e` are aliases, not new behaviour. `ctrl-e` is in the catalog's
            // `RESERVED_KEYS` because the *table* binds it, and no table is live while a modal
            // text input has the keys - which is also why nothing is added to `RESERVED_KEYS`.
            Key::Home | Key::Ctrl('a') => {
                self.home();
                Edit::Moved
            }
            Key::End | Key::Ctrl('e') => {
                self.end();
                Edit::Moved
            }
            _ => return None,
        };
        Some(edit)
    }
}

impl std::ops::Deref for Input {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

/// Not a convenience: `palette.rs`'s own tests construct `Palette { input: "q".into(), .. }` and
/// assign `p.input = "recovery".into()`, and without these two impls every such literal in the
/// suite would have to be rewritten (design §8.1).
impl From<&str> for Input {
    fn from(text: &str) -> Input {
        Input {
            cursor: text.len(),
            text: text.to_string(),
        }
    }
}

impl From<String> for Input {
    fn from(text: String) -> Input {
        Input {
            cursor: text.len(),
            text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cursor is where the next character lands, and it moves with what it inserted.
    #[test]
    fn a_character_lands_where_the_cursor_stands() {
        let mut i = Input::from("subnet");
        assert_eq!(i.cursor(), 6, "`From` puts it at the end");
        assert!(i.at_end());
        assert!(i.left() && i.left() && i.left());
        assert_eq!(i.cursor(), 3);
        assert!(!i.at_end());
        i.insert('X');
        assert_eq!(i.as_str(), "subXnet");
        assert_eq!(i.cursor(), 4, "after what it inserted");
        i.insert_str("YZ");
        assert_eq!(i.as_str(), "subXYZnet");
        assert_eq!(i.cursor(), 6);
    }

    /// Backspace deletes what is *before* the cursor, and answers `false` at the left edge so a
    /// widget can report `Ignored` rather than a pointless `Edited`.
    #[test]
    fn backspace_deletes_before_the_cursor_and_stops_at_the_edge() {
        let mut i = Input::from("abc");
        assert!(i.backspace());
        assert_eq!((i.as_str(), i.cursor()), ("ab", 2));
        i.home();
        assert_eq!(i.cursor(), 0);
        assert!(!i.backspace(), "nothing before the start");
        assert_eq!(i.as_str(), "ab");
        assert!(!i.left(), "and nowhere to go");
        i.end();
        assert!(i.at_end());
        assert!(!i.right());
    }

    /// Readline's `ctrl-w`: the spaces left of the cursor, then the run of non-spaces.
    #[test]
    fn ctrl_w_takes_the_word_before_the_cursor() {
        let mut i = Input::from("ctx prod-eu");
        assert!(i.delete_word());
        assert_eq!(i.as_str(), "ctx ");
        assert!(
            i.delete_word(),
            "the trailing space goes with the word before it"
        );
        assert_eq!(i.as_str(), "");
        assert!(!i.delete_word(), "and then there is nothing to take");

        // Mid-string: only the word before the cursor goes, and the tail is untouched.
        let mut i = Input::from("can-i power-off vm");
        assert!(i.left() && i.left() && i.left());
        assert_eq!(i.cursor(), 15, "just after `power-off`");
        assert!(i.delete_word());
        assert_eq!(i.as_str(), "can-i  vm");
        assert_eq!(i.cursor(), 6);
    }

    /// `ctrl-u` clears back to the start and leaves everything after the cursor alone.
    #[test]
    fn ctrl_u_clears_back_to_the_start() {
        let mut i = Input::from("skin gruvbox-dark");
        for _ in 0..5 {
            assert!(i.left());
        }
        assert!(i.delete_to_start());
        assert_eq!((i.as_str(), i.cursor()), ("-dark", 0));
        assert!(!i.delete_to_start(), "already there");
    }

    /// A context name may not be ASCII, and a byte cursor that landed inside a character would
    /// panic on the next slice. Every mover steps whole characters.
    #[test]
    fn the_cursor_only_ever_stands_on_a_char_boundary() {
        let mut i = Input::from("café-eu");
        i.home();
        for _ in 0..7 {
            assert!(i.right());
            assert!(i.as_str().is_char_boundary(i.cursor()), "{}", i.cursor());
        }
        assert!(!i.right());
        assert_eq!(i.cursor(), 8, "seven characters, eight bytes");
        for _ in 0..7 {
            assert!(i.left());
            assert!(i.as_str().is_char_boundary(i.cursor()));
        }
        assert_eq!(i.cursor(), 0);
        // And deleting one takes the whole character.
        i.end();
        assert!(i.backspace() && i.backspace() && i.backspace() && i.backspace());
        assert_eq!(i.as_str(), "caf");
    }

    /// `set` is history recall: the whole line is replaced and the cursor goes to the end.
    #[test]
    fn set_replaces_the_line_and_ends_at_the_end() {
        let mut i = Input::from("half-typed");
        i.home();
        i.set("ctx lab".to_string());
        assert_eq!((i.as_str(), i.cursor()), ("ctx lab", 7));
        assert!(i.at_end());
        assert!(!i.is_empty());
        i.set(String::new());
        assert!(i.is_empty() && i.at_end() && i.cursor() == 0);
    }

    /// `Deref` keeps `&input` usable wherever a `&str` is wanted, and the two `From` impls are
    /// what keep `Palette { input: "q".into(), .. }` compiling unchanged (design §8.1).
    #[test]
    fn it_reads_as_a_str_and_is_built_from_one() {
        fn takes(s: &str) -> usize {
            s.len()
        }
        let i: Input = "ctx".into();
        assert_eq!(takes(&i), 3);
        assert!(i.starts_with("ct"), "through Deref");
        let j: Input = String::from("ctx").into();
        assert_eq!(i, j);
        assert_eq!(Input::default(), Input::from(""));
    }

    /// The left-scroll guard: a line longer than the room drops characters from the left, and
    /// says so, so the cursor is always on screen. Ratatui clips the right edge itself, which is
    /// why nothing here looks at the tail.
    #[test]
    fn a_line_too_long_for_the_room_scrolls_from_the_left() {
        let i = Input::from("abcdef");
        assert_eq!(i.window(20), (false, 0), "it fits: draw all of it");
        // Four cells: one for the `…`, one for the block cursor, two for text.
        assert_eq!(i.window(4), (true, 4), "…ef█");
        let mut j = Input::from("abcdef");
        j.home();
        assert_eq!(j.window(4), (false, 0), "the cursor is already at the left");
    }

    /// A combining mark, a variation selector and a zero-width joiner take no cell of their own,
    /// so they are not a step for the cursor: it can never land inside what the terminal draws as
    /// one cell, which is what would let a reversed cell cover half of one.
    #[test]
    fn the_cursor_steps_over_whole_clusters() {
        let mut i = Input::from("ae\u{301}b");
        assert_eq!(i.cursor(), 5, "four scalars, five bytes");
        assert!(i.left());
        assert_eq!(i.cursor(), 4, "before `b`");
        assert!(i.left());
        assert_eq!(i.cursor(), 1, "past the whole `e´`, never into it");
        assert!(i.left());
        assert_eq!(i.cursor(), 0);
        assert!(!i.left());
        assert!(i.right() && i.right());
        assert_eq!(i.cursor(), 4, "and back over it in one step");

        // A ZWJ sequence is one cluster however many scalars it has.
        let mut i = Input::from("x\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}y");
        i.home();
        assert!(i.right());
        assert_eq!(i.cursor(), 1, "past `x`");
        assert!(i.right());
        assert_eq!(i.cursor(), 19, "the whole family, in one step");
        assert!(i.right());
        assert!(i.at_end());
    }

    /// `cluster_end` is the rule the cursor and the drawing both read, so the two cannot disagree
    /// about what one cell holds.
    #[test]
    fn a_cluster_is_a_character_plus_what_takes_no_cell_of_its_own() {
        assert_eq!(cluster_end("abc", 0), 1);
        assert_eq!(cluster_end("e\u{301}b", 0), 3, "the mark comes with it");
        assert_eq!(cluster_end("字b", 0), 3, "a wide character is still one");
        assert_eq!(
            cluster_end("\u{1F44D}\u{FE0F}!", 0),
            7,
            "a variation selector too"
        );
        assert_eq!(cluster_end("abc", 3), 3, "nothing at the end");
        assert_eq!(cluster_end("", 0), 0);
    }

    /// One key map for four widgets: `Changed` for a key that changed the text, `Moved` for one
    /// that only moved the cursor, `Ignored` for one this type owns that had nowhere to go, and
    /// `None` for a key it does not own at all - which the widget then answers for itself.
    #[test]
    fn one_key_map_answers_for_every_widget() {
        let mut i = Input::from("ctx lab");
        assert_eq!(i.key(Key::Char('!')), Some(Edit::Changed));
        assert_eq!(i.as_str(), "ctx lab!");
        assert_eq!(i.key(Key::Backspace), Some(Edit::Changed));
        assert_eq!(i.as_str(), "ctx lab");
        assert_eq!(i.key(Key::Left), Some(Edit::Moved));
        assert_eq!(i.key(Key::Home), Some(Edit::Moved));
        assert_eq!(i.key(Key::Ctrl('a')), Some(Edit::Moved));
        assert_eq!(i.key(Key::Left), Some(Edit::Ignored), "nowhere to go");
        assert_eq!(i.key(Key::End), Some(Edit::Moved));
        assert_eq!(i.key(Key::Ctrl('e')), Some(Edit::Moved));
        assert_eq!(i.key(Key::Right), Some(Edit::Ignored));
        assert_eq!(i.key(Key::Ctrl('w')), Some(Edit::Changed));
        assert_eq!(i.as_str(), "ctx ");
        assert_eq!(i.key(Key::Ctrl('u')), Some(Edit::Changed));
        assert!(i.is_empty());
        // A deletion with nothing to delete is still `Changed`, exactly as `String::pop` on an
        // empty input always was: the widget re-ranks, finds the same rows, draws the same frame.
        assert_eq!(i.key(Key::Backspace), Some(Edit::Changed));
        for key in [
            Key::Enter,
            Key::Esc,
            Key::Tab,
            Key::BackTab,
            Key::Up,
            Key::Down,
            Key::PageUp,
            Key::Ctrl('p'),
        ] {
            assert_eq!(i.key(key), None, "{key:?} is the widget's");
        }
    }
}
