//! The `?` overlay.

use super::*;

/// The box the `?` overlay is drawn in, borders included, and the one number this file will
/// not negotiate: `help_draws_over_the_view_it_was_opened_from` renders at 100×27, where the
/// body is 18 rows, and a box taller than that clamps and covers the title the test asserts
/// on. So the interior is `HELP_BOX.1 - 2` rows by `HELP_BOX.0 - 2` cells, `HELP` fills both
/// exactly, and `the_help_text_fits_its_box` measures it - `Paragraph` clips rather than
/// wraps and reports nothing when it does.
pub(super) const HELP_BOX: (u16, u16) = (70, 16);

/// The keys, in fourteen lines of sixty-eight cells.
///
/// The mouse rows are here and nowhere else: no prompt line advertises a gesture, so over SSH
/// or in a terminal with reporting off the app reads identically - and "why can't I copy the
/// error message" is the first bug report that feature will produce, which is why `^o` and
/// shift-drag are named here rather than left to be found.
///
/// `a` and `space` are here for the opposite reason: they were named nowhere, and twenty-five
/// VM actions sat behind them. Room for two lines came from folding `esc back / close` onto
/// the `enter` row and `ctrl-r refresh now` onto the `?` row - the inline second key that rows
/// 4, 6 and 14 already use - rather than from a fifteenth line, which would have vanished.
///
/// `/` cost `w` its own line, which is now the second key on the `S` row: the two of them
/// reshape the table between them, and `w wide` is also in the header's hint grid where `/`
/// was in neither. Same idiom again, and still fourteen lines.
///
/// `:search` then cost nothing: it shares the `/` row, because the two of them are one gesture
/// at two reaches and the sentence that distinguishes them is the same sentence - these rows,
/// or every loaded kind. What it did cost is `esc clears it`, which `esc back / close` two rows
/// up already half says.
///
/// The command list on row 2 is a selection, not an index: it names the commands nothing else
/// on the overlay names. So `search` left it when the `/` row arrived and `help` left it when
/// `?` was spelled out at the bottom, and `settings` took the room that freed.
///
/// `^x` cost nothing either, and the room came from the box spelling one modifier two ways:
/// `^w` on row 3 against `ctrl-o`, `ctrl-r` and `ctrl-c` at the bottom. One spelling throughout,
/// the same `^` the header's hint grid uses, pays for `^x stop the walk` on the last row
/// with cells to spare, and reads as one vocabulary instead of two. `^t schedule` then fitted
/// beside them, and the last row lost only the words `now` and `the walk`.
///
/// `^p ^n history` cost `⏎ run` its place on row 3. Of the four keys on that row it is the one
/// a person guesses without being told - `enter` runs the thing under the cursor in every list
/// this program draws - where `^w` and `^p`/`^n` are named here or nowhere.
pub(super) const HELP: &str = "\
:            palette: a kind, a page, or a command
             (ctx, skin, settings, journal, activity, export, can-i)
             ⇥ complete · ↑↓ pick · ^p ^n history · ^w delete word
j k g G      move        PgUp PgDn   page
enter        drill into children, or detail     esc   back / close
y            detail (composed)  Y  raw YAML  J  raw JSON  w  watch
a            actions for this row; the menu shows each one's key
space        mark a row; a then acts on every marked row
S            sort by next column, then reverse   w  wide: 20 columns
/  :search   match name, IP or id here, or across every loaded kind
mouse        click a row, a menu item or a header; the wheel scrolls
             the pane under it; shift-drag still selects text
^o           mouse capture off for this session; :mouse, for good
?            this help  ^r refresh  ^x stop  ^t schedule  ^c quit";

pub(super) fn draw_help(f: &mut Frame, body: Rect) {
    let area = centered_rect_with_min(0, 0, HELP_BOX.0, HELP_BOX.1, body);
    clear_region(f, area);
    f.render_widget(
        Paragraph::new(HELP).block(Block::bordered().title("keys")),
        area,
    );
}
