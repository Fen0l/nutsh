//! Input the app understands, independent of crossterm so tests need no terminal.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
}
