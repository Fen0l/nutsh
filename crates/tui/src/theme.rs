//! Data-driven palette and semantic styles for the TUI.
//!
//! The palette is a 25-swatch [`Palette`] (Catppuccin's slot layout). A named built-in skin is
//! selected in config, with optional per-swatch hex overrides:
//!
//! ```toml
//! [skin]
//! name = "gruvbox-dark"
//!
//! [skin.colors]        # optional; keys are swatch names (see `Palette` fields)
//! red   = "#fb4934"
//! mauve = "#d3869b"
//! ```
//!
//! [`install`] resolves and installs the palette once at startup, before the alternate screen
//! (the OSC 11 query writes to the tty); [`apply_named`] replaces it live for `:skin`. Every
//! accessor reads a per-thread copy tagged with an epoch, so a frame's hundreds of colour
//! lookups cost one relaxed atomic load and a stack copy rather than an `RwLock` acquire each.
//!
//! ---------------------------------------------------------------------------
//! Third-party code
//!
//! `Palette`, the `palette_swatches!` macro, the seventeen built-in skin tables, the `[skin]`
//! config shape, `parse_hex`, `resolve_skin`, `validate_skin`, `auto_skin_name`, the
//! `ACTIVE`/`EPOCH`/`CACHED` triple with `init`/`set`/`snapshot`, `set_background`/
//! `background` and the chrome accessors are taken from **sofka**
//! (<https://github.com/nklmilojevic/sofka>), licensed **MIT OR Apache-2.0**.
//! Copyright (c) 2026 Nikola Milojević.
//!
//! Four things are ours. `role_fg`/`row_fg` over [`nutsh_catalog::Role`] replace sofka's
//! `status_color`/`row_color`/`severity_fg`, which matched Kubernetes phase strings inside the
//! theme; our vocabulary lives in `nutsh_core::status` and is per-kind overridable, so the
//! theme only maps an enum. [`Depth`] downgrades every swatch once at resolve time, so no
//! accessor and no view ever branches on the terminal's colour depth. `resolve_skin` is
//! silent: the binary prints [`validate_skin`]'s warnings before the alternate screen, and a
//! warning written while the TUI owns the screen would land in the middle of a frame. And
//! [`canonical`] holds the aliases sofka kept inside `builtin`'s match arms, so the name
//! `:skin` records and hands the config is always a [`BUILTIN_NAMES`] spelling.
//! ---------------------------------------------------------------------------

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use nutsh_catalog::Role;
use ratatui::style::{Color, Modifier, Style};
use terminal_colorsaurus::{QueryOptions, ThemeMode};

macro_rules! palette_swatches {
    ($($idx:expr => $field:ident),+ $(,)?) => {
        /// A full 25-swatch palette. Field names double as the override keys accepted under
        /// `[skin.colors]` in config.
        #[derive(Debug, Clone, Copy)]
        pub struct Palette {
            $(pub $field: Color,)+
        }

        /// Swatch names in the order the built-in tables list their hex values.
        const FIELDS: &[&str] = &[$(stringify!($field),)+];

        impl Palette {
            /// Build a palette from 25 hex strings in [`FIELDS`] order. Panics on a malformed
            /// value - only ever called with the hardcoded built-in tables, so a bad hex here
            /// is a programmer error, not user input.
            fn from_hexes(h: &[&str]) -> Palette {
                assert_eq!(
                    h.len(),
                    FIELDS.len(),
                    "built-in palette has {} colors but {} swatches are defined",
                    h.len(),
                    FIELDS.len()
                );
                let c = |i: usize| {
                    parse_hex(h[i]).unwrap_or_else(|| panic!("bad built-in hex {}", h[i]))
                };
                Palette {
                    $($field: c($idx),)+
                }
            }

            /// Override one swatch by name. Returns `false` for an unknown key.
            fn set(&mut self, key: &str, color: Color) -> bool {
                match key {
                    $(stringify!($field) => self.$field = color,)+
                    _ => return false,
                }
                true
            }

            /// Every swatch through `f`, in place: how the colour-depth downgrade is applied
            /// once, at resolve time.
            fn map(&mut self, f: impl Fn(Color) -> Color) {
                $(self.$field = f(self.$field);)+
            }
        }

        $(pub fn $field() -> Color {
            palette().$field
        })+
    };
}

palette_swatches! {
    0 => rosewater,
    1 => flamingo,
    2 => pink,
    3 => mauve,
    4 => red,
    5 => maroon,
    6 => peach,
    7 => yellow,
    8 => green,
    9 => teal,
    10 => sky,
    11 => sapphire,
    12 => blue,
    13 => lavender,
    14 => text,
    15 => subtext1,
    16 => subtext0,
    17 => overlay1,
    18 => overlay0,
    19 => surface2,
    20 => surface1,
    21 => surface0,
    22 => base,
    23 => mantle,
    24 => crust,
}

// ---------------------------------------------------------------------------
// Built-in skins
// ---------------------------------------------------------------------------

/// Names accepted by [`builtin`] (canonical spellings), for help and error text.
pub const BUILTIN_NAMES: &[&str] = &[
    "catppuccin-mocha",
    "catppuccin-latte",
    "catppuccin-frappe",
    "catppuccin-macchiato",
    "gruvbox-dark",
    "gruvbox-light",
    "nord",
    "dracula",
    "solarized-dark",
    "solarized-light",
    "tokyo-night",
    "one-dark",
    "rose-pine",
    "rose-pine-dawn",
    "monokai",
    "flexoki-dark",
    "flexoki-light",
];

/// Every alias a skin name may be typed as, as `(alias, canonical)`: the short Catppuccin
/// flavour names and sofka's hyphen-free spellings.
///
/// [`canonical`] is implemented over this, so the two cannot drift - there is no second place an
/// alias could be written - and the palette matches against it so that `:skin onedark` completes
/// instead of silently working only on `enter`.
pub(crate) const SKIN_ALIASES: &[(&str, &str)] = &[
    ("flexoki", "flexoki-dark"),
    ("frappe", "catppuccin-frappe"),
    ("gruvbox", "gruvbox-dark"),
    ("latte", "catppuccin-latte"),
    ("macchiato", "catppuccin-macchiato"),
    ("mocha", "catppuccin-mocha"),
    ("onedark", "one-dark"),
    ("rosepine", "rose-pine"),
    ("rosepinedawn", "rose-pine-dawn"),
    ("solarized", "solarized-dark"),
    ("tokyonight", "tokyo-night"),
];

/// The [`BUILTIN_NAMES`] spelling of `name`, or `None` for one nothing knows: trimmed,
/// lowercased, and the aliases of `SKIN_ALIASES` resolved. [`builtin`] matches the canonical
/// spellings, so a name it accepts is always one `:skin`'s list can mark and the config can be
/// handed back.
pub fn canonical(name: &str) -> Option<&'static str> {
    let key = name.trim().to_ascii_lowercase();
    if let Some(name) = BUILTIN_NAMES.iter().find(|n| **n == key) {
        return Some(name);
    }
    SKIN_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, name)| *name)
}

/// Look up a built-in palette by name, through [`canonical`], so case and the aliases are
/// accepted here too.
pub fn builtin(name: &str) -> Option<Palette> {
    // FIELDS order: rosewater, flamingo, pink, mauve, red, maroon, peach, yellow, green, teal,
    // sky, sapphire, blue, lavender, text, subtext1, subtext0, overlay1, overlay0, surface2,
    // surface1, surface0, base, mantle, crust
    let hex: [&str; 25] = match canonical(name)? {
        "catppuccin-mocha" => [
            "#f5e0dc", "#f2cdcd", "#f5c2e7", "#cba6f7", "#f38ba8", "#eba0ac", "#fab387", "#f9e2af",
            "#a6e3a1", "#94e2d5", "#89dceb", "#74c7ec", "#89b4fa", "#b4befe", "#cdd6f4", "#bac2de",
            "#a6adc8", "#7f849c", "#6c7086", "#585b70", "#45475a", "#313244", "#1e1e2e", "#181825",
            "#11111b",
        ],
        "catppuccin-latte" => [
            "#dc8a78", "#dd7878", "#ea76cb", "#8839ef", "#d20f39", "#e64553", "#fe640b", "#df8e1d",
            "#40a02b", "#179299", "#04a5e5", "#209fb5", "#1e66f5", "#7287fd", "#4c4f69", "#5c5f77",
            "#6c6f85", "#8c8fa1", "#9ca0b0", "#acb0be", "#bcc0cc", "#ccd0da", "#eff1f5", "#e6e9ef",
            "#dce0e8",
        ],
        "catppuccin-frappe" => [
            "#f2d5cf", "#eebebe", "#f4b8e4", "#ca9ee6", "#e78284", "#ea999c", "#ef9f76", "#e5c890",
            "#a6d189", "#81c8be", "#99d1db", "#85c1dc", "#8caaee", "#babbf1", "#c6d0f5", "#b5bfe2",
            "#a5adce", "#949cbb", "#838ba7", "#626880", "#51576d", "#414559", "#303446", "#292c3c",
            "#232634",
        ],
        "catppuccin-macchiato" => [
            "#f4dbd6", "#f0c6c6", "#f5bde6", "#c6a0f6", "#ed8796", "#ee99a0", "#f5a97f", "#eed49f",
            "#a6da95", "#8bd5ca", "#91d7e3", "#7dc4e4", "#8aadf4", "#b7bdf8", "#cad3f5", "#b8c0e0",
            "#a5adcb", "#8087a2", "#6e738d", "#5b6078", "#494d64", "#363a4f", "#24273a", "#1e2030",
            "#181926",
        ],
        "gruvbox-dark" => [
            "#ebdbb2", "#d5c4a1", "#d3869b", "#d3869b", "#fb4934", "#cc241d", "#fe8019", "#fabd2f",
            "#b8bb26", "#8ec07c", "#83a598", "#458588", "#83a598", "#d3869b", "#ebdbb2", "#d5c4a1",
            "#bdae93", "#a89984", "#928374", "#665c54", "#504945", "#3c3836", "#282828", "#1d2021",
            "#1d2021",
        ],
        "nord" => [
            "#d8dee9", "#e5e9f0", "#b48ead", "#b48ead", "#bf616a", "#bf616a", "#d08770", "#ebcb8b",
            "#a3be8c", "#8fbcbb", "#88c0d0", "#81a1c1", "#5e81ac", "#b48ead", "#eceff4", "#e5e9f0",
            "#d8dee9", "#616e88", "#4c566a", "#434c5e", "#3b4252", "#333a47", "#2e3440", "#2b303b",
            "#242933",
        ],
        "dracula" => [
            "#f8f8f2", "#ffb86c", "#ff79c6", "#bd93f9", "#ff5555", "#ff5555", "#ffb86c", "#f1fa8c",
            "#50fa7b", "#8be9fd", "#8be9fd", "#62d6e8", "#6272a4", "#bd93f9", "#f8f8f2", "#d8d8d2",
            "#b8b8b2", "#6272a4", "#565761", "#44475a", "#3a3c4e", "#343746", "#282a36", "#21222c",
            "#191a21",
        ],
        "gruvbox-light" => [
            "#ebdbb2", "#d5c4a1", "#b16286", "#b16286", "#cc241d", "#9d0006", "#d65d0e", "#d79921",
            "#98971a", "#689d6a", "#458588", "#076678", "#458588", "#b16286", "#282828", "#3c3836",
            "#504945", "#665c54", "#7c6f64", "#928374", "#a89984", "#bdae93", "#fbf1c7", "#ebdbb2",
            "#d5c4a1",
        ],
        "solarized-dark" => [
            "#d33682", "#d33682", "#d33682", "#6c71c4", "#dc322f", "#dc322f", "#cb4b16", "#b58900",
            "#859900", "#2aa198", "#2aa198", "#268bd2", "#268bd2", "#6c71c4", "#93a1a1", "#839496",
            "#657b83", "#586e75", "#586e75", "#586e75", "#073642", "#073642", "#002b36", "#002b36",
            "#001e26",
        ],
        "solarized-light" => [
            "#d33682", "#d33682", "#d33682", "#6c71c4", "#dc322f", "#dc322f", "#cb4b16", "#b58900",
            "#859900", "#2aa198", "#2aa198", "#268bd2", "#268bd2", "#6c71c4", "#002b36", "#073642",
            "#586e75", "#657b83", "#839496", "#93a1a1", "#eee8d5", "#eee8d5", "#fdf6e3", "#eee8d5",
            "#eee8d5",
        ],
        "tokyo-night" => [
            "#f7768e", "#f7768e", "#bb9af7", "#9d7cd8", "#f7768e", "#db4b4b", "#ff9e64", "#e0af68",
            "#9ece6a", "#73daca", "#7dcfff", "#0db9d7", "#7aa2f7", "#9d7cd8", "#c0caf5", "#a9b1d6",
            "#545c7e", "#545c7e", "#3b4261", "#3b4261", "#292e42", "#292e42", "#1a1b26", "#16161e",
            "#13131a",
        ],
        "one-dark" => [
            "#e06c75", "#e06c75", "#c678dd", "#c678dd", "#e06c75", "#be5046", "#d19a66", "#e5c07b",
            "#98c379", "#56b6c2", "#56b6c2", "#61afef", "#61afef", "#c678dd", "#abb2bf", "#828997",
            "#5c6370", "#5c6370", "#4b5263", "#4b5263", "#3b4048", "#323842", "#282c34", "#21252b",
            "#1b1d23",
        ],
        "rose-pine" => [
            "#ebbcba", "#ebbcba", "#ebbcba", "#c4a7e7", "#eb6f92", "#eb6f92", "#f6c177", "#f6c177",
            "#31748f", "#31748f", "#9ccfd8", "#9ccfd8", "#9ccfd8", "#c4a7e7", "#e0def4", "#908caa",
            "#6e6a86", "#6e6a86", "#524f67", "#524f67", "#403d52", "#26233a", "#191724", "#191724",
            "#14121d",
        ],
        "rose-pine-dawn" => [
            "#d7827e", "#d7827e", "#d7827e", "#907aa9", "#b4637a", "#b4637a", "#ea9d34", "#ea9d34",
            "#286983", "#286983", "#56949f", "#56949f", "#56949f", "#907aa9", "#575279", "#797593",
            "#9893a5", "#9893a5", "#cecacd", "#cecacd", "#dfdad9", "#f2e9e1", "#faf4ed", "#faf4ed",
            "#f4ede8",
        ],
        // Flexoki (https://stephango.com/flexoki): dark uses the 400 accents on the
        // black/base-950..800 surface ramp, light the 600 accents on paper/base-50..200, per
        // the palette's own theme role tables.
        "flexoki-dark" => [
            "#E47DA8", "#CE5D97", "#CE5D97", "#8B7EC8", "#D14D41", "#AF3029", "#DA702C", "#D0A215",
            "#879A39", "#3AA99F", "#5ABDAC", "#66A0C8", "#4385BE", "#A699D0", "#CECDC3", "#B7B5AC",
            "#878580", "#6F6E69", "#575653", "#403E3C", "#343331", "#282726", "#100F0F", "#1C1B1A",
            "#100F0F",
        ],
        "flexoki-light" => [
            "#CE5D97", "#A02F6F", "#A02F6F", "#5E409D", "#AF3029", "#C03E35", "#BC5215", "#AD8301",
            "#66800B", "#24837B", "#2F968D", "#3171B2", "#205EA6", "#735EB5", "#100F0F", "#575653",
            "#6F6E69", "#878580", "#9F9D96", "#B7B5AC", "#CECDC3", "#DAD8CE", "#FFFCF0", "#F2F0E5",
            "#E6E4D9",
        ],
        "monokai" => [
            "#f92672", "#f92672", "#f92672", "#ae81ff", "#f92672", "#f92672", "#fd971f", "#e6db74",
            "#a6e22e", "#66d9ef", "#66d9ef", "#66d9ef", "#66d9ef", "#ae81ff", "#f8f8f2", "#cfcfc2",
            "#75715e", "#75715e", "#49483e", "#49483e", "#3e3d32", "#3e3d32", "#272822", "#23241f",
            "#1e1f1c",
        ],
        _ => return None,
    };
    Some(Palette::from_hexes(&hex))
}

fn catppuccin_mocha() -> Palette {
    builtin("catppuccin-mocha").expect("mocha built-in")
}

/// Best-effort dark/light detection via an OSC 11 background-colour query. `None` means the
/// terminal did not answer (unsupported, `TERM=dumb`, piped output, or the query timed out) -
/// callers treat that as "assume dark" rather than blocking or erroring.
fn detect_terminal_mode() -> Option<ThemeMode> {
    terminal_colorsaurus::theme_mode(QueryOptions::default()).ok()
}

/// The skin used when config names none: auto-detected from the terminal's dark/light mode.
/// Run before entering the alternate screen.
pub fn auto_skin_name() -> &'static str {
    match detect_terminal_mode() {
        Some(ThemeMode::Light) => "catppuccin-latte",
        _ => "catppuccin-mocha",
    }
}

/// Resolve a palette: the named built-in (falling back to mocha), then the per-swatch hex
/// overrides, then the colour-depth downgrade over every swatch.
///
/// Silent by design - see the module header. [`validate_skin`] is the half that reports, and
/// [`install`] the one that asks the terminal when the config names no skin.
pub fn resolve_skin(name: &str, colors: &BTreeMap<String, String>) -> Palette {
    let mut p = builtin(name).unwrap_or_else(catppuccin_mocha);
    for (k, v) in colors {
        let key = k.trim().to_ascii_lowercase();
        if let Some(c) = parse_hex(v) {
            p.set(&key, c);
        }
    }
    let d = depth();
    if d != Depth::Truecolor {
        p.map(|c| downgrade(c, d));
    }
    p
}

/// Check a skin selection without applying it: an unknown built-in name, an unknown swatch key
/// under `[skin.colors]`, or a malformed hex value. One human-readable warning per problem
/// (sorted for stable output), empty when the selection is clean. `source` is the config key
/// the name came from - `skin.name`, or `contexts.<name>.skin` - so the warning points at the
/// line to fix.
pub fn validate_skin(
    name: Option<&str>,
    colors: &BTreeMap<String, String>,
    source: &str,
) -> Vec<String> {
    let mut warns = Vec::new();
    if let Some(n) = name
        && canonical(n).is_none()
    {
        warns.push(format!(
            "{source}: unknown skin '{n}' (known: {}); using catppuccin-mocha",
            BUILTIN_NAMES.join(", ")
        ));
    }
    let mut probe = catppuccin_mocha();
    for (k, v) in colors {
        let key = k.trim().to_ascii_lowercase();
        match parse_hex(v) {
            Some(c) if probe.set(&key, c) => {}
            Some(_) => warns.push(format!("skin.colors.{k}: unknown swatch name")),
            None => warns.push(format!(
                "skin.colors.{k}: invalid hex '{v}' (expected #rrggbb)"
            )),
        }
    }
    warns.sort();
    warns
}

/// Parse `#rrggbb` (or `rrggbb`) into a `Color::Rgb`. `None` for any other length or non-hex
/// content.
fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

// ---------------------------------------------------------------------------
// Colour depth
// ---------------------------------------------------------------------------

/// How many colours the terminal can show. Resolved once at startup and applied once, at
/// resolve time, so no accessor and no view ever branches on it - and `:skin` under a
/// 16-colour terminal still switches to a recognisably different palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Truecolor,
    Ansi256,
    Ansi16,
    None,
}

/// Settable, not a `OnceLock`, for the same reason the skin is: a `OnceLock` would let the
/// first test that ran decide the depth for every other one.
static DEPTH: AtomicU8 = AtomicU8::new(0);

pub fn set_depth(d: Depth) {
    DEPTH.store(
        match d {
            Depth::Truecolor => 0,
            Depth::Ansi256 => 1,
            Depth::Ansi16 => 2,
            Depth::None => 3,
        },
        Ordering::Relaxed,
    );
}

pub fn depth() -> Depth {
    match DEPTH.load(Ordering::Relaxed) {
        1 => Depth::Ansi256,
        2 => Depth::Ansi16,
        3 => Depth::None,
        _ => Depth::Truecolor,
    }
}

/// The depth this terminal implies. See [`depth_from`] for the order.
pub fn detect_depth() -> Depth {
    depth_from(
        std::env::var("NO_COLOR").ok().as_deref(),
        std::env::var("NUTSH_COLOR").ok().as_deref(),
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// `NUTSH_COLOR=truecolor|256|16|none` overrides everything (an unrecognised value is
/// ignored); then a non-empty `NO_COLOR` is `None`; then `COLORTERM` containing `truecolor` or
/// `24bit` is `Truecolor`; then a `TERM` ending `-256color` is `Ansi256`; else `Ansi16`.
///
/// Arguments rather than the environment so tests need no `set_var`, which is unsound with
/// other threads running.
pub fn depth_from(
    no_color: Option<&str>,
    nutsh_color: Option<&str>,
    colorterm: Option<&str>,
    term: Option<&str>,
) -> Depth {
    if let Some(v) = nutsh_color {
        match v.trim().to_ascii_lowercase().as_str() {
            "truecolor" | "24bit" => return Depth::Truecolor,
            "256" => return Depth::Ansi256,
            "16" => return Depth::Ansi16,
            "none" | "0" => return Depth::None,
            _ => {}
        }
    }
    if no_color.is_some_and(|v| !v.is_empty()) {
        return Depth::None;
    }
    if colorterm.is_some_and(|v| v.contains("truecolor") || v.contains("24bit")) {
        return Depth::Truecolor;
    }
    if term.is_some_and(|v| v.ends_with("-256color")) {
        return Depth::Ansi256;
    }
    Depth::Ansi16
}

fn downgrade(c: Color, depth: Depth) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    match depth {
        Depth::Truecolor => c,
        Depth::Ansi256 => Color::Indexed(nearest_ansi256(r, g, b)),
        Depth::Ansi16 => nearest_ansi16(r, g, b),
        Depth::None => Color::Reset,
    }
}

fn distance((r1, g1, b1): (u8, u8, u8), (r2, g2, b2): (u8, u8, u8)) -> u32 {
    let d = |a: u8, b: u8| {
        let d = i32::from(a) - i32::from(b);
        d.unsigned_abs().pow(2)
    };
    d(r1, r2) + d(g1, g2) + d(b1, b2)
}

/// The nearest of the 6×6×6 cube and the 24-step grey ramp, by squared RGB distance.
fn nearest_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|&(_, &l)| distance((l, 0, 0), (v, 0, 0)))
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube_err = distance((LEVELS[ri], LEVELS[gi], LEVELS[bi]), (r, g, b));
    // The ramp runs 8, 18, …, 238: index by the mean channel, clamped to its 24 steps.
    let mean = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    let grey_i = (mean.saturating_sub(8) / 10).min(23);
    let grey = u8::try_from(8 + 10 * grey_i).unwrap_or(u8::MAX);
    if distance((grey, grey, grey), (r, g, b)) < cube_err {
        u8::try_from(232 + grey_i).unwrap_or(u8::MAX)
    } else {
        u8::try_from(16 + 36 * ri + 6 * gi + bi).unwrap_or(u8::MAX)
    }
}

/// The nearest of the sixteen named colours. A desaturated swatch can land on a grey, which is
/// what sixteen colours are.
fn nearest_ansi16(r: u8, g: u8, b: u8) -> Color {
    const ANSI16: [(Color, (u8, u8, u8)); 16] = [
        (Color::Black, (0, 0, 0)),
        (Color::Red, (170, 0, 0)),
        (Color::Green, (0, 170, 0)),
        (Color::Yellow, (170, 85, 0)),
        (Color::Blue, (0, 0, 170)),
        (Color::Magenta, (170, 0, 170)),
        (Color::Cyan, (0, 170, 170)),
        (Color::Gray, (170, 170, 170)),
        (Color::DarkGray, (85, 85, 85)),
        (Color::LightRed, (255, 85, 85)),
        (Color::LightGreen, (85, 255, 85)),
        (Color::LightYellow, (255, 255, 85)),
        (Color::LightBlue, (85, 85, 255)),
        (Color::LightMagenta, (255, 85, 255)),
        (Color::LightCyan, (85, 255, 255)),
        (Color::White, (255, 255, 255)),
    ];
    ANSI16
        .iter()
        .min_by_key(|(_, rgb)| distance(*rgb, (r, g, b)))
        .map(|(c, _)| *c)
        .unwrap_or(Color::Reset)
}

// ---------------------------------------------------------------------------
// Active palette and accessors
// ---------------------------------------------------------------------------

static ACTIVE: OnceLock<RwLock<Palette>> = OnceLock::new();

/// Bumped whenever the active palette changes. Readers compare it against their thread-local
/// copy; equal means the copy is current. Starts at 1 so a cache initialised at epoch 0 always
/// misses on first use.
static EPOCH: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Per-thread copy of the active palette, tagged with the epoch it was taken at. A frame
    /// takes hundreds of colour lookups; an `RwLock` acquire per lookup is waste.
    static CACHED: Cell<(u64, Palette)> = Cell::new((0, catppuccin_mocha()));
}

/// Install the active palette. After startup it replaces the active one, so `:skin` updates
/// colours without restarting the TUI.
pub fn init(p: Palette) {
    if ACTIVE.set(RwLock::new(p)).is_err() {
        set(p);
    }
    EPOCH.fetch_add(1, Ordering::Release);
}

/// Replace the active palette. Private: [`init`] is the installer views and the binary call,
/// and [`apply_named`] the live switch, so no view can pick the wrong one of two.
fn set(p: Palette) {
    let lock = ACTIVE.get_or_init(|| RwLock::new(catppuccin_mocha()));
    match lock.write() {
        Ok(mut active) => *active = p,
        Err(poisoned) => *poisoned.into_inner() = p,
    }
    // After the write, so any thread that observes the new epoch also observes the new palette.
    EPOCH.fetch_add(1, Ordering::Release);
}

/// The active palette, as a snapshot. Prefer this over several individual swatch accessors.
pub fn snapshot() -> Palette {
    palette()
}

fn palette() -> Palette {
    let epoch = EPOCH.load(Ordering::Acquire);
    CACHED.with(|c| {
        let (cached_epoch, cached) = c.get();
        if cached_epoch == epoch {
            return cached;
        }
        let fresh = load_active();
        c.set((epoch, fresh));
        fresh
    })
}

fn load_active() -> Palette {
    let lock = ACTIVE.get_or_init(|| RwLock::new(catppuccin_mocha()));
    match lock.read() {
        Ok(active) => *active,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// Whether views are filled with the skin's own background (`base`) rather than left
/// transparent. Independent of the active palette so it survives a live `:skin`.
static BACKGROUND: AtomicBool = AtomicBool::new(false);

pub fn set_background(on: bool) {
    BACKGROUND.store(on, Ordering::Relaxed);
}

/// The background fill colour when enabled, or `None` to leave the terminal's alone.
pub fn background() -> Option<Color> {
    BACKGROUND.load(Ordering::Relaxed).then(base)
}

/// The `[skin.colors]` overrides the config carried, kept so a live `:skin` re-applies them on
/// top of the new built-in. The TUI never parses config; the binary sets this once.
static OVERRIDES: OnceLock<BTreeMap<String, String>> = OnceLock::new();

fn overrides() -> BTreeMap<String, String> {
    OVERRIDES.get().cloned().unwrap_or_default()
}

/// The name of the skin now showing, for `:skin`'s list and for `--info`.
static CURRENT: OnceLock<RwLock<String>> = OnceLock::new();

fn set_current(name: &str) {
    let lock = CURRENT.get_or_init(|| RwLock::new(String::new()));
    match lock.write() {
        Ok(mut current) => *current = name.to_string(),
        Err(poisoned) => *poisoned.into_inner() = name.to_string(),
    }
}

pub fn current_name() -> String {
    match CURRENT.get().map(|l| l.read()) {
        Some(Ok(name)) => name.clone(),
        Some(Err(poisoned)) => poisoned.into_inner().clone(),
        None => "catppuccin-mocha".to_string(),
    }
}

/// What the binary installs at startup: the depth, the `[skin]` selection and its overrides,
/// in one call, so the TUI can re-resolve on `:skin` without ever seeing the config.
///
/// Run before `enable_raw_mode`: [`auto_skin_name`] writes an OSC 11 query to the tty.
pub fn install(name: Option<&str>, colors: &BTreeMap<String, String>, background: bool) {
    set_depth(detect_depth());
    let _ = OVERRIDES.set(colors.clone());
    let resolved = match name {
        Some(n) => canonical(n).unwrap_or("catppuccin-mocha"),
        None => auto_skin_name(),
    };
    set_current(resolved);
    set_background(background);
    init(resolve_skin(resolved, colors));
}

/// `:skin NAME`: apply a built-in live, keeping the configured overrides on top of it, and
/// answer its canonical name - what the list marks and the config is handed, so an alias
/// (`gruvbox`, `mocha`) is never recorded as the current skin. `Err` is the sentence to put on
/// the status line; nothing changes in that case.
pub fn apply_named(name: &str) -> Result<&'static str, String> {
    let Some(canonical) = canonical(name) else {
        return Err(format!(
            "unknown skin '{name}' (known: {})",
            BUILTIN_NAMES.join(", ")
        ));
    };
    set(resolve_skin(canonical, &overrides()));
    set_current(canonical);
    Ok(canonical)
}

// ---------------------------------------------------------------------------
// Semantic styles
// ---------------------------------------------------------------------------

// The frame styles below mirror k9s' catppuccin skin element for element (`frame.title`,
// `frame.border`, `views.table.*`), so the chrome matches.

/// Border-title text: k9s `frame.title.fgColor` (teal), bold.
pub fn title() -> Style {
    Style::default().fg(teal()).add_modifier(Modifier::BOLD)
}

/// Unfocused border: k9s `frame.border.fgColor` (mauve).
pub fn border() -> Style {
    Style::default().fg(mauve())
}

/// Focused border: k9s `frame.border.focusColor` (lavender).
pub fn border_focused() -> Style {
    Style::default().fg(lavender())
}

/// Table header: k9s `views.table.header.fgColor` (yellow), non-bold.
pub fn header_row() -> Style {
    Style::default().fg(yellow())
}

/// Cursor / selected row: a bright lavender bar with dark text, bold.
pub fn selected_row() -> Style {
    Style::default()
        .bg(lavender())
        .fg(base())
        .add_modifier(Modifier::BOLD)
}

/// Sort-indicator arrow in the header: k9s `header.sorterColor` (sky).
pub fn sorter() -> Color {
    sky()
}

/// Title `[count]` colour: k9s `frame.title.counterColor` (yellow).
pub fn counter() -> Color {
    yellow()
}

/// Marked-row colour: k9s `views.table.markColor` (rosewater).
pub fn mark() -> Color {
    rosewater()
}

pub fn dim() -> Style {
    Style::default().fg(overlay1())
}

pub fn accent() -> Style {
    Style::default().fg(teal())
}

/// The status cell's badge colour. Badges are bold (the caller adds the modifier) so the cell
/// still stands out inside a row already tinted by [`row_fg`].
pub fn role_fg(r: Role) -> Color {
    match r {
        Role::Ok => green(),
        // The user's explicit exception: OFF is worth spotting, a powered-off VM is not a
        // broken one - so the cell is red and the row (below) stays blue.
        Role::Off => red(),
        Role::Pending => peach(),
        Role::Error => red(),
        Role::Warn => yellow(),
        Role::Info => sky(),
        Role::Ending => mauve(),
        Role::Muted => overlay1(),
        Role::Neutral => blue(),
    }
}

/// The whole row's tint, k9s' model: a table where every row is green says nothing, so healthy
/// rows take the standard blue and only the states worth seeing move.
pub fn row_fg(r: Role) -> Color {
    match r {
        // `StdColor`.
        Role::Ok | Role::Off | Role::Neutral => blue(),
        Role::Pending => peach(),
        Role::Error => red(),
        Role::Warn => yellow(),
        // An INFO alert is not a row-level problem.
        Role::Info => blue(),
        // k9s' `killColor`: CANCELED, DELETING, TO_BE_REMOVED.
        Role::Ending => mauve(),
        Role::Muted => overlay0(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test in this module mutates the process-global palette, so each takes this first.
    static SKIN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn locked() -> std::sync::MutexGuard<'static, ()> {
        let guard = SKIN.lock().unwrap_or_else(|e| e.into_inner());
        set_depth(Depth::Truecolor);
        guard
    }

    // sofka's own tests are transcribed here under the attribution of the module header,
    // minus `row_color_matches_k9s_model`, `status_color_fades_terminal_states...`,
    // `helm_release_statuses_are_colored` and `severity_maps_to_warn_and_critical_tints`,
    // whose subjects (`row_color`, `status_color`, `severity_fg`) this module does not have;
    // the role tests in `crates/tui/tests/theme.rs` replace them.

    #[test]
    fn all_builtins_parse() {
        let _g = locked();
        for name in BUILTIN_NAMES {
            assert!(builtin(name).is_some(), "missing built-in {name}");
        }
    }

    /// Every spelling `builtin` accepts comes back as the one in `BUILTIN_NAMES`, which is what
    /// `:skin`'s list marks and what the config is handed back.
    #[test]
    fn canonical_folds_case_and_aliases_onto_builtin_names() {
        for name in BUILTIN_NAMES {
            assert_eq!(canonical(name), Some(*name));
        }
        // Only what the alias list itself cannot say: case, and surrounding space. The eleven
        // pairs live in `SKIN_ALIASES` and `every_skin_alias_resolves_to_a_built_in` walks them
        // there, so spelling them again here would be the second place an alias is written.
        for (alias, name) in [("Gruvbox", "gruvbox-dark"), (" mocha ", "catppuccin-mocha")] {
            assert_eq!(canonical(alias), Some(name), "{alias}");
            assert!(BUILTIN_NAMES.contains(&name), "{name} is in the list");
        }
        assert_eq!(canonical("no-such-skin"), None);
        assert!(builtin("no-such-skin").is_none());
    }

    /// The per-thread palette cache must not outlive a `:skin` change. This is the one hazard
    /// the caching introduces, so it gets a test: read a colour, swap the palette, read again.
    #[test]
    fn live_skin_change_invalidates_the_cached_palette() {
        let _g = locked();
        let mocha = builtin("catppuccin-mocha").unwrap();
        let dawn = builtin("rose-pine-dawn").unwrap();
        assert_ne!(mocha.base, dawn.base, "test needs two distinct palettes");

        set(mocha);
        assert_eq!(base(), mocha.base);
        let _ = (text(), red(), green());

        set(dawn);
        assert_eq!(base(), dawn.base, "stale palette served after :skin");
        assert_eq!(text(), dawn.text);

        set(mocha);
        assert_eq!(base(), mocha.base, "stale palette served on switch back");
    }

    #[test]
    fn mocha_base_matches_golden_swatch() {
        let _g = locked();
        assert_eq!(
            builtin("catppuccin-mocha").unwrap().base,
            Color::Rgb(30, 30, 46)
        );
    }

    #[test]
    fn rose_pine_dawn_swatches_match_palette_spec() {
        let _g = locked();
        let dawn = builtin("rose-pine-dawn").unwrap();
        assert_eq!(dawn.base, Color::Rgb(0xFA, 0xF4, 0xED));
        assert_eq!(dawn.text, Color::Rgb(0x57, 0x52, 0x79));
        assert_eq!(dawn.red, Color::Rgb(0xB4, 0x63, 0x7A));
        assert_eq!(builtin("rosepinedawn").unwrap().base, dawn.base);
    }

    #[test]
    fn flexoki_swatches_match_palette_spec() {
        let _g = locked();
        let dark = builtin("flexoki-dark").unwrap();
        assert_eq!(dark.base, Color::Rgb(0x10, 0x0F, 0x0F));
        assert_eq!(dark.red, Color::Rgb(0xD1, 0x4D, 0x41));
        assert_eq!(dark.text, Color::Rgb(0xCE, 0xCD, 0xC3));
        assert_eq!(builtin("flexoki").unwrap().base, dark.base);
        let light = builtin("flexoki-light").unwrap();
        assert_eq!(light.base, Color::Rgb(0xFF, 0xFC, 0xF0));
        assert_eq!(light.red, Color::Rgb(0xAF, 0x30, 0x29));
        assert_eq!(light.text, Color::Rgb(0x10, 0x0F, 0x0F));
    }

    #[test]
    fn parse_hex_forms() {
        assert_eq!(parse_hex("#1e1e2e"), Some(Color::Rgb(30, 30, 46)));
        assert_eq!(parse_hex("1e1e2e"), Some(Color::Rgb(30, 30, 46)));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gggggg"), None);
    }

    #[test]
    fn resolve_applies_overrides_and_ignores_junk() {
        let _g = locked();
        let mut colors = BTreeMap::new();
        colors.insert("red".to_string(), "#010203".to_string());
        colors.insert("nope".to_string(), "#040506".to_string()); // unknown key
        colors.insert("green".to_string(), "zzzzzz".to_string()); // bad hex
        let p = resolve_skin("catppuccin-mocha", &colors);
        assert_eq!(p.red, Color::Rgb(1, 2, 3));
        assert_eq!(p.green, builtin("catppuccin-mocha").unwrap().green);
    }

    #[test]
    fn validate_skin_reports_each_problem_and_passes_clean_config() {
        let mut colors = BTreeMap::new();
        colors.insert("red".to_string(), "#010203".to_string());
        assert!(validate_skin(Some("gruvbox-dark"), &colors, "skin.name").is_empty());
        assert!(validate_skin(None, &BTreeMap::new(), "skin.name").is_empty());

        colors.insert("nope".to_string(), "#040506".to_string());
        colors.insert("green".to_string(), "zzzzzz".to_string());
        let warns = validate_skin(Some("no-such-skin"), &colors, "skin.name");
        assert_eq!(warns.len(), 3, "{warns:?}");
        assert!(
            warns
                .iter()
                .any(|w| w.contains("skin.name") && w.contains("no-such-skin"))
        );
        assert!(
            warns
                .iter()
                .any(|w| w.contains("skin.colors.nope") && w.contains("unknown swatch"))
        );
        assert!(
            warns
                .iter()
                .any(|w| w.contains("skin.colors.green") && w.contains("invalid hex"))
        );

        // A name from a context's own `skin` is reported under that key, not `skin.name`.
        let warns = validate_skin(Some("no-such-skin"), &BTreeMap::new(), "contexts.lab.skin");
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(warns[0].starts_with("contexts.lab.skin: "), "{warns:?}");
    }

    #[test]
    fn unknown_skin_falls_back_to_mocha() {
        let _g = locked();
        let p = resolve_skin("no-such-skin", &BTreeMap::new());
        assert_eq!(p.base, catppuccin_mocha().base);
    }

    #[test]
    fn background_is_off_until_asked_for() {
        let _g = locked();
        set_background(false);
        assert_eq!(background(), None);
        set_background(true);
        assert_eq!(background(), Some(base()));
        set_background(false);
    }

    /// `canonical` is implemented *over* `SKIN_ALIASES`, so the two cannot drift: there is no
    /// second place an alias could be spelled. This pins the other half - every alias resolves
    /// to a name that is really in the list, none shadows a canonical spelling, all are stored
    /// folded, because `canonical` folds the input before it looks, and each paints the palette
    /// its canonical name paints.
    #[test]
    fn every_skin_alias_resolves_to_a_built_in() {
        assert!(!SKIN_ALIASES.is_empty());
        for (alias, name) in SKIN_ALIASES {
            assert_eq!(canonical(alias), Some(*name), "{alias}");
            assert!(BUILTIN_NAMES.contains(name), "{name} is in the list");
            assert!(
                !BUILTIN_NAMES.contains(alias),
                "{alias} shadows a canonical spelling"
            );
            assert_eq!(
                *alias,
                alias.to_ascii_lowercase(),
                "{alias} is stored folded"
            );
            assert_eq!(
                builtin(alias).map(|p| p.base),
                builtin(name).map(|p| p.base),
                "{alias} is {name}"
            );
        }
        // The four the palette could not reach before: none is a substring of its own canonical
        // name, so `BUILTIN_NAMES.contains(needle)` never found them.
        for alias in ["tokyonight", "onedark", "rosepine", "rosepinedawn"] {
            let name = canonical(alias).expect("an alias");
            assert!(
                !name.contains(alias),
                "{alias} would have been found by substring alone"
            );
        }
    }
}
