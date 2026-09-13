//! ratatui views and the mode state machine. `App` never touches the terminal; `run` does.

pub mod app;
pub mod confirm;
pub mod contexts;
pub mod detail;
pub mod form;
pub mod journal;
pub mod key;
pub mod menu;
pub mod mouse;
pub mod page;
pub mod palette;
pub mod picker;
pub mod run;
pub mod search;
pub mod settings;
pub mod sidebar;
pub mod skins;
pub mod table;
pub mod text;
pub mod theme;
pub mod ui;

pub use app::App;
pub use key::Key;
pub use mouse::{Mouse, MouseKind};
pub use run::run;

/// Prism Central's port. It is implied wherever it is shown - an address with it spelled out
/// says nothing an address without it does not - and it is what the add form starts on.
pub const DEFAULT_PORT: u16 = 9440;
