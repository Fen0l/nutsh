//! The theme's public surface. sofka's own unit tests travel with the module (they reach
//! `parse_hex`, which stays private); these are the ones this project adds.

mod common;

use nutsh_catalog::Role;
use nutsh_tui::theme::{self, Depth};
use ratatui::style::Color;

/// Mocha at truecolour under the lock every test that touches the palette or the depth holds.
fn locked() -> std::sync::MutexGuard<'static, ()> {
    common::pin_skin()
}

#[test]
fn every_builtin_parses_and_the_names_are_the_documented_seventeen() {
    let _g = locked();
    assert_eq!(theme::BUILTIN_NAMES.len(), 17);
    for name in theme::BUILTIN_NAMES {
        assert!(theme::builtin(name).is_some(), "missing built-in {name}");
    }
    // The short Catppuccin flavour names are accepted too.
    assert_eq!(
        theme::builtin("mocha").map(|p| p.base),
        theme::builtin("catppuccin-mocha").map(|p| p.base)
    );
}

/// One test per role: the badge colour and the row tint, from the design's table.
#[test]
fn every_role_has_its_badge_and_its_row_tint() {
    let _g = locked();
    let p = theme::snapshot();
    for (role, badge, row) in [
        (Role::Ok, p.green, p.blue),
        (Role::Off, p.red, p.blue),
        (Role::Pending, p.peach, p.peach),
        (Role::Error, p.red, p.red),
        (Role::Warn, p.yellow, p.yellow),
        (Role::Info, p.sky, p.blue),
        (Role::Ending, p.mauve, p.mauve),
        (Role::Muted, p.overlay1, p.overlay0),
        (Role::Neutral, p.blue, p.blue),
    ] {
        assert_eq!(theme::role_fg(role), badge, "{role:?} badge");
        assert_eq!(theme::row_fg(role), row, "{role:?} row");
    }
}

/// The one rule not derivable from the others, and the user's explicit choice: OFF is worth
/// spotting, a powered-off VM is not a broken one.
#[test]
fn off_reddens_the_cell_but_not_the_row() {
    let _g = locked();
    let p = theme::snapshot();
    assert_eq!(theme::role_fg(Role::Off), p.red);
    assert_eq!(theme::row_fg(Role::Off), p.blue);
    assert_eq!(theme::row_fg(Role::Off), theme::row_fg(Role::Ok));
}

/// A 16-colour terminal still gets a recognisably different palette, and `NO_COLOR` gets a
/// fully laid-out frame with no colour at all. No accessor branches on the depth: the
/// downgrade happens once, at resolve time.
#[test]
fn the_depth_downgrade_maps_every_swatch_once() {
    let _g = locked();
    let colors = std::collections::BTreeMap::new();

    theme::set_depth(Depth::Ansi256);
    let mocha = theme::resolve_skin("catppuccin-mocha", &colors);
    assert_eq!(mocha.red, Color::Indexed(211), "#f38ba8 is nearest 211");
    assert!(matches!(mocha.base, Color::Indexed(_)));

    theme::set_depth(Depth::Ansi16);
    let gruvbox = theme::resolve_skin("gruvbox-dark", &colors);
    assert_eq!(gruvbox.red, Color::LightRed, "#fb4934 is a bright red");

    theme::set_depth(Depth::None);
    let none = theme::resolve_skin("catppuccin-mocha", &colors);
    assert_eq!(none.red, Color::Reset);
    assert_eq!(none.base, Color::Reset);
}

/// Resolution order: `NUTSH_COLOR` overrides everything, then `NO_COLOR`, then `COLORTERM`,
/// then `TERM`. Read from arguments rather than from the environment so the test needs no
/// `set_var`, which is unsound with other threads running.
#[test]
fn depth_is_detected_from_the_environment_in_one_order() {
    use theme::depth_from;
    assert_eq!(
        depth_from(Some("1"), Some("truecolor"), None, None),
        Depth::Truecolor
    );
    assert_eq!(
        depth_from(Some("1"), None, Some("truecolor"), None),
        Depth::None
    );
    assert_eq!(
        depth_from(Some(""), None, Some("truecolor"), None),
        Depth::Truecolor
    );
    assert_eq!(
        depth_from(None, Some("16"), Some("truecolor"), None),
        Depth::Ansi16
    );
    assert_eq!(
        depth_from(None, Some("nonsense"), None, None),
        Depth::Ansi16
    );
    assert_eq!(
        depth_from(None, None, Some("24bit"), None),
        Depth::Truecolor
    );
    assert_eq!(
        depth_from(None, None, None, Some("xterm-256color")),
        Depth::Ansi256
    );
    assert_eq!(depth_from(None, None, None, Some("xterm")), Depth::Ansi16);
    assert_eq!(depth_from(None, None, None, None), Depth::Ansi16);
}

/// `:skin` applies a built-in live and keeps the `[skin.colors]` overrides on top of it; an
/// unknown name changes nothing and says what is known; an alias is recorded under the name
/// in `BUILTIN_NAMES`, so the list can mark it and the config gets a documented spelling.
#[test]
fn apply_named_switches_live_and_refuses_a_name_it_does_not_know() {
    let _g = locked();
    let before = theme::snapshot().base;
    let err = theme::apply_named("no-such-skin").unwrap_err();
    assert!(
        err.contains("no-such-skin") && err.contains("gruvbox-dark"),
        "{err}"
    );
    assert_eq!(theme::snapshot().base, before, "nothing changed");

    assert_eq!(theme::apply_named("gruvbox-dark"), Ok("gruvbox-dark"));
    assert_eq!(
        theme::snapshot().base,
        theme::builtin("gruvbox-dark").unwrap().base
    );
    assert_eq!(theme::current_name(), "gruvbox-dark");

    theme::apply_named("catppuccin-mocha").unwrap();
    assert_eq!(theme::apply_named(" Gruvbox "), Ok("gruvbox-dark"));
    assert_eq!(
        theme::snapshot().base,
        theme::builtin("gruvbox-dark").unwrap().base
    );
    assert_eq!(theme::current_name(), "gruvbox-dark");

    theme::apply_named("catppuccin-mocha").unwrap();
    assert_eq!(theme::snapshot().base, before);
}
