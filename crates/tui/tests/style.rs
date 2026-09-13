//! What the frame is coloured with. Text snapshots cannot see a style, so these read the
//! buffer.
//!
//! The selected row is the lavender bar of `selected_row()`, patched over its cells after they
//! are drawn, so a cell's own colour is read off a row the cursor is not on.

mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::{App, Key};
use ratatui::style::Modifier;
use serde_json::json;

async fn app(pc: &MockPc) -> App {
    app_of(pc, "vm").await
}

/// The settled table of `kind`: `app` is the VM one, and the columns its curated set lacks -
/// a timestamp, a heading longer than its cap - are read off other tables. The mock answers
/// every catalog path, with a fixture's rows where it has one and an empty list where it does
/// not, and a heading is drawn either way.
async fn app_of(pc: &MockPc, kind: &str) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        kind,
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app
}

/// The one rule not derivable from the others: OFF is worth spotting, a powered-off VM is not
/// a broken one, so the cell is red and the row stays the standard blue.
#[tokio::test]
async fn off_reddens_the_cell_but_not_the_row() {
    let pc = MockPc::builder().start().await;
    let app = app(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 19).unwrap();
    assert_eq!(common::style_of(&buf, "web-02").fg, Some(p.blue));
    let off = common::style_of(&buf, "OFF");
    assert_eq!(off.fg, Some(p.red));
    assert!(off.add_modifier.contains(Modifier::BOLD), "badges are bold");
    assert_eq!(common::style_of(&buf, "db-01").fg, Some(p.blue));
}

/// One row per role word: the badge takes `role_fg`, the row takes `row_fg`, and only the
/// badge is bold, so the status cell still stands out inside its own row.
#[tokio::test]
async fn every_role_tints_its_row_and_its_badge() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    // Row 0 takes the cursor bar, whose style covers the row's own, so it is a row of its own
    // and the nine role rows are all read unselected.
    app.inject_entities(vec![
        json!({"extId": "0", "name": "cursor-row", "powerState": "ON"}),
        json!({"extId": "1", "name": "role-ok", "powerState": "RUNNING"}),
        json!({"extId": "2", "name": "role-off", "powerState": "OFF"}),
        json!({"extId": "3", "name": "role-pending", "powerState": "MIGRATING"}),
        json!({"extId": "4", "name": "role-error", "powerState": "FAILED"}),
        json!({"extId": "5", "name": "role-warn", "powerState": "WARNING"}),
        json!({"extId": "6", "name": "role-info", "powerState": "INFO"}),
        json!({"extId": "7", "name": "role-ending", "powerState": "DELETING"}),
        json!({"extId": "8", "name": "role-muted", "powerState": "$UNKNOWN"}),
        json!({"extId": "9", "name": "role-neutral", "powerState": "BANANA"}),
    ]);
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 23).unwrap();
    for (name, word, badge, row) in [
        ("role-ok", "RUNNING", p.green, p.blue),
        ("role-off", "OFF", p.red, p.blue),
        ("role-pending", "MIGRATING", p.peach, p.peach),
        ("role-error", "FAILED", p.red, p.red),
        ("role-warn", "WARNING", p.yellow, p.yellow),
        ("role-info", "INFO", p.sky, p.blue),
        ("role-ending", "DELETING", p.mauve, p.mauve),
        // The wire word is `$UNKNOWN`; a `Status` badge strips the `$` before it shouts, and
        // `status::role_in` normalises it away too, so the tint is the one the wire asked for.
        ("role-muted", "UNKNOWN", p.overlay1, p.overlay0),
        ("role-neutral", "BANANA", p.blue, p.blue),
    ] {
        assert_eq!(common::style_of(&buf, word).fg, Some(badge), "{name} badge");
        assert!(
            common::style_of(&buf, word)
                .add_modifier
                .contains(Modifier::BOLD),
            "{name} badge is bold"
        );
        assert_eq!(common::style_of(&buf, name).fg, Some(row), "{name} row");
        assert!(
            !common::style_of(&buf, name)
                .add_modifier
                .contains(Modifier::BOLD),
            "{name} row is not bold"
        );
    }
}

/// The chrome: a lavender focused border,
/// a teal bold title with a yellow count, a yellow header row with a sky sort arrow, a
/// lavender selection bar, and dim ages.
#[tokio::test]
async fn the_table_chrome_is_the_k9s_one() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 19).unwrap();
    // The table's corner, not the header box's above it, which is the unfocused mauve.
    assert_eq!(
        common::style_of(&buf, "╭ Virtual").fg,
        Some(p.lavender),
        "the body has focus"
    );
    // The title, not the header line above it, which names the kind too.
    assert_eq!(
        common::style_of(&buf, "Virtual Machines [").fg,
        Some(p.teal)
    );
    assert_eq!(common::style_of(&buf, "[3]").fg, Some(p.yellow));
    assert_eq!(common::style_of(&buf, "POWER").fg, Some(p.yellow));
    let bar = common::style_of(&buf, "▌ ");
    assert_eq!(bar.bg, Some(p.lavender));

    app.handle(Key::Char('S'));
    let buf = app.frame(120, 19).unwrap();
    assert_eq!(
        common::style_of(&buf, "↑").fg,
        Some(p.sky),
        "the sorter is sky"
    );
    let text = common::text_of(&buf);
    assert!(text.contains("NAME ↑"), "{text}");

    // The arrow is not only on the one column with cells to spare. `S` cycles each column
    // ascending then descending, so two more presses land on POWER, a `Cap` column sized to
    // its own heading, which grows by the mark rather than losing its last letters.
    for _ in 0..2 {
        app.handle(Key::Char('S'));
    }
    let buf = app.frame(120, 19).unwrap();
    let text = common::text_of(&buf);
    assert!(text.contains("POWER ↑  CLUSTER"), "{text}");
    assert_eq!(common::style_of(&buf, "↑").fg, Some(p.sky));
    app.handle(Key::Char('S'));
    let text = app.snapshot(120, 19).unwrap();
    assert!(text.contains("POWER ↓  CLUSTER"), "{text}");
}

/// Two things the curated VM table cannot show. Ages are dim: its columns hold no timestamp,
/// so the Tasks table's STARTED is read. A heading its `Cap` cannot hold whole is cut at the
/// twelve-cell floor and still gives the sort mark its last cells: the VPC's fourth column is
/// `EXTERNAL SUBNETS`, a `Count`, whose `Cap(5)` is raised to the heading's own floor of twelve
/// and no further, so the heading loses its last four cells and the mark keeps its two. Read
/// off a curated kind on purpose, so that curating another kind cannot take the column away.
#[tokio::test]
async fn ages_are_dim_and_a_cut_heading_keeps_its_sort_mark() {
    let pc = MockPc::builder().start().await;
    let tasks = app_of(&pc, "task").await;
    let mut vpcs = app_of(&pc, "networking.config.Vpc").await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = tasks.frame(120, 19).unwrap();
    assert_eq!(
        common::style_of(&buf, "58s").fg,
        Some(p.overlay1),
        "ages are dim"
    );
    // Two presses per column, ascending then descending: seven land on the fourth ascending.
    for _ in 0..7 {
        vpcs.handle(Key::Char('S'));
    }
    let buf = vpcs.frame(120, 19).unwrap();
    let text = common::text_of(&buf);
    assert!(text.contains("EXTERNAL SUB ↑  ROUTABLE"), "{text}");
    // Read out of the body: the menu's own border carries an `↑n` count of what it has
    // scrolled past, and it is on an earlier row.
    assert_eq!(
        common::style_of_within(&buf, MENU_WIDTH..buf.area.width, "↑").fg,
        Some(p.sky)
    );
}

/// The dim flag on screen: an unresolved reference and an empty cell are drawn in
/// `theme::dim()`, and a resolved name is not.
///
/// The rows are injected rather than polled, so the assertion does not depend on what the
/// warm-up managed: `deadbeef-…` is a cluster no fixture has and nothing will ever name it.
/// The cursor is parked on a third row, because the selection bar paints over the two rows
/// that are being read.
#[tokio::test]
async fn an_unresolved_reference_is_dim_and_a_name_is_not() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_names(&mut app).await;
    app.inject_entities(vec![
        json!({
            "extId": "3d0c4a2e-1b8f-4c1a-9e2f-0000000000fe",
            "name": "anchor",
            "powerState": "ON",
            "cluster": {"extId": "0006158a-2f0d-4d5a-8e2d-000000000010"},
        }),
        json!({
            "extId": "3d0c4a2e-1b8f-4c1a-9e2f-0000000000ff",
            "name": "orphan",
            "powerState": "ON",
            "cluster": {"extId": "deadbeef-0000-4000-8000-000000000000"},
        }),
        json!({
            "extId": "3d0c4a2e-1b8f-4c1a-9e2f-0000000000fd",
            "name": "parked",
            "powerState": "ON",
        }),
    ]);
    app.handle(Key::Down);
    app.handle(Key::Down);
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    // Sixteen rows, so all three are in the window at once: the body is the height less the
    // seven-row header, the two borders, the headings and the two prompt lines.
    let buf = app.frame(120, 16).unwrap();
    assert_eq!(
        common::style_of(&buf, "deadbeef").fg,
        Some(p.overlay1),
        "the eight-character stub is dim"
    );
    assert_eq!(
        common::style_of(&buf, "lab-cluster").fg,
        Some(p.blue),
        "a resolved name takes the row's own tint - `theme::row_fg(Role::Ok)` is blue"
    );
    // The HOST column of every row is missing, so it is the dim dash. Read from the `HOST`
    // heading rightwards: the header box's own `Context: -` is mauve, and the resolved
    // `lab-cluster` in the column before carries a hyphen of its own.
    assert_eq!(
        common::style_of_within(&buf, common::column_band(&buf, "HOST"), "-").fg,
        Some(p.overlay1)
    );
}

/// The mark `space` puts in front of a row is the one glyph on the frame the user placed
/// themselves, so it never reads as the absence of a value: it clears the first cell's `dim`
/// rather than inheriting it. The row it is on has no `name`, so without that the whole cell -
/// mark included - would be `theme::dim()`.
#[tokio::test]
async fn a_mark_is_never_dim() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    // The nameless row is the middle one: the cursor starts on the first and the selection bar
    // paints over the cells it is on, and `space` moves the cursor down onto the third, so the
    // marked row is read in its own colours either way.
    app.inject_entities(vec![
        json!({"extId": "3d0c4a2e-1b8f-4c1a-9e2f-0000000000fa", "name": "anchor", "powerState": "ON"}),
        json!({"extId": "3d0c4a2e-1b8f-4c1a-9e2f-0000000000fb", "powerState": "ON"}),
        json!({"extId": "3d0c4a2e-1b8f-4c1a-9e2f-0000000000fc", "name": "parked", "powerState": "ON"}),
    ]);
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 15).unwrap();
    // The `NAME` column alone: every other cell of these rows is a dash too, and the two named
    // rows are there to keep the cursor off the one being read.
    let name_column =
        common::column_band(&buf, "NAME").start..common::column_band(&buf, "POWER").start;
    assert_eq!(
        common::style_of_within(&buf, name_column, "-").fg,
        Some(p.overlay1),
        "the nameless row's identity cell is dim"
    );
    app.handle(Key::Down);
    app.handle(Key::Char(' '));
    let buf = app.frame(120, 15).unwrap();
    assert_eq!(
        common::style_of(&buf, "* ").fg,
        Some(p.blue),
        "and the mark on it is not"
    );
}

/// `←`/`→` scroll the columns under the anchored first one, and the title says so. Text
/// only, so it takes no skin guard: the columns are the same in every palette.
#[tokio::test]
async fn the_title_counts_hidden_columns() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    // 43 cells of body, one short of the six floors: one column is dropped, and the title
    // says `»1`.
    let narrow = app.snapshot(45, 19).unwrap();
    assert!(narrow.contains("»1"), "{narrow}");
    assert!(!narrow.contains("MEM"), "{narrow}");

    app.handle(Key::Right);
    let scrolled = app.snapshot(120, 19).unwrap();
    assert!(scrolled.contains("‹1"), "{scrolled}");
    assert!(!scrolled.contains("POWER"), "{scrolled}");
    assert!(
        scrolled.contains("NAME"),
        "the first column is anchored: {scrolled}"
    );
    app.handle(Key::Left);
    assert!(
        !app.snapshot(120, 19).unwrap().contains("‹"),
        "back at the left edge"
    );
}

/// Eighty columns, the narrowest terminal worth supporting: the six curated VM columns fit
/// with room, every heading whole and nothing dropped. Forty-six is where their floors - 8 +
/// 5 + 7 + 4 + 4 + 4, plus the gutter and the spacing - are exactly the 44 cells there are:
/// still nothing is dropped, but the Flex shares land under their floors and the reserves
/// overflow the budget, and `table::fit` lifts each column to its floor and pays the excess
/// back, so the header ends at the border - ratatui had nothing to squeeze. Text only, so no
/// skin guard.
///
/// The warm-up is settled first: a `Reference` is `Cap(18)`, so what the `CLUSTER` and `HOST`
/// cells hold is what those two columns are worth, and asserting the eight-character stubs
/// would be asserting that the warm-up lost a race it is meant to win.
#[tokio::test]
async fn an_eighty_column_frame_keeps_every_heading_whole_and_a_tight_one_at_its_floor() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_names(&mut app).await;
    let text = app.snapshot(80, 17).unwrap();
    assert!(
        text.contains(
            "│  NAME                    POWER  CLUSTER      HOST        IP            MEM   │"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "│▌ web-01                  ON     lab-cluster  ahv-node-1  203.0.113.11  8 GiB │"
        ),
        "{text}"
    );
    assert!(!text.contains('»'), "nothing dropped: {text}");

    let text = app.snapshot(46, 17).unwrap();
    assert!(
        text.contains("│  NAME      POWER  CLUSTER  HOST  IP    MEM │"),
        "{text}"
    );
    assert!(
        text.contains("│▌ web-01    ON     lab-clu  ahv-  203.  8 Gi│"),
        "{text}"
    );
    assert!(!text.contains('»'), "nothing dropped: {text}");
}

/// The header's colours: a mauve box with a teal title, dim labels, a mauve context with its
/// flags, a sapphire PC, a green cluster, a peach kind, and a sky bold hint key.
#[tokio::test]
async fn the_header_box_is_coloured_by_field() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    app.live.as_mut().unwrap().session.readonly = true;
    app.live.as_mut().unwrap().session.cluster = Some(nutsh_prism::ClusterRef {
        ext_id: "0006-cafe".into(),
        name: "lab-cluster".into(),
    });
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 14).unwrap();
    assert_eq!(common::style_of(&buf, " nutsh ").fg, Some(p.teal));
    assert_eq!(common::style_of(&buf, "Context:").fg, Some(p.overlay1));
    assert_eq!(common::style_of(&buf, "[read-only]").fg, Some(p.red));
    assert_eq!(common::style_of(&buf, "pc.2024.3").fg, Some(p.sapphire));
    assert_eq!(common::style_of(&buf, "lab-cluster").fg, Some(p.green));
    // `Kind:` is the menu's path to what is open, not the kind's display name on its own.
    assert_eq!(
        common::style_of(&buf, "Compute & Storage › VMs").fg,
        Some(p.peach)
    );
    // `^c` rather than `^b`: `a actions` took `^b menu`'s cell, and the prompt line's dim
    // `^b:menu` is what a bare `^b` would find instead. `^c` is on the grid and nowhere else.
    assert_eq!(common::style_of(&buf, "^c").fg, Some(p.sky));
    assert_eq!(common::style_of(&buf, "● live").fg, Some(p.green));
    assert_eq!(common::style_of(&buf, "clusters").fg, Some(p.overlay1));
}

/// The popup's chrome: the title in the shared title colour, the prompt's lead mauve, a
/// greyed row's reason dim.
#[tokio::test]
async fn the_popup_uses_the_shared_chrome() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    app.handle(Key::Char(':'));
    // `nics`, not `vm`: it ranks three rows, two of them greyed, so the dim reason this test
    // reads is near the top of the popup rather than its twelfth row - and the assertion stops
    // depending on where the catalog ranks a kind or on how tall the terminal is.
    for c in "nics".chars() {
        app.handle(Key::Char(c));
    }
    let buf = app.frame(100, 24).unwrap();
    assert_eq!(common::style_of(&buf, "kinds & commands").fg, Some(p.teal));
    assert_eq!(
        common::style_of(&buf, ":nics").fg,
        Some(p.mauve),
        "the prompt colon"
    );
    // The second greyed row, named in full: the popup's first row is the selected one, and the
    // selection bar paints over the colours this assertion reads.
    assert_eq!(
        common::style_of(&buf, "open from Hosts").fg,
        Some(p.overlay1)
    );
}

/// The menu's own colours, which the text snapshots discard: the open item takes the accent,
/// its count the counter yellow, an item the v4 API has no equivalent for is greyed, and the
/// border says which pane the arrow keys will move.
///
/// The needles are read out of the menu's 24 cells rather than the whole frame: the body's own
/// block is titled `VMs [3]` too, and it is on the menu's first row.
#[tokio::test]
async fn the_menu_marks_the_open_item_and_greys_the_missing_one() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let menu = 0..MENU_WIDTH;

    let buf = app.frame(100, 30).unwrap();
    assert_eq!(
        common::style_of_within(&buf, menu.clone(), "VMs").fg,
        Some(p.teal),
        "the open item takes the accent"
    );
    assert_eq!(
        common::style_of_within(&buf, menu.clone(), " [3]").fg,
        Some(p.yellow),
        "and its row count the counter colour"
    );
    assert_eq!(
        common::style_of_within(&buf, menu.clone(), "Catalog Items").fg,
        Some(p.overlay1),
        "a feature the v4 API has no equivalent for is greyed"
    );
    assert_eq!(
        common::style_of_within(&buf, menu.clone(), "Compute & Storage").fg,
        Some(p.overlay1),
        "a group header is dim"
    );
    assert_eq!(menu_border(&buf), Some(p.mauve), "the keys are in the body");

    app.handle(Key::Tab);
    let buf = app.frame(100, 30).unwrap();
    assert_eq!(
        menu_border(&buf),
        Some(p.lavender),
        "tab moves them into the menu"
    );
}

/// A stale block is dim throughout, so a screen of numbers never looks live when it is not.
///
/// The cycle is settled first: with every counter still `None` the block is already `⚠ -` in
/// `overlay1`, since a zero warns nobody, and the assertion would pass whether or not `stale`
/// did anything.
#[tokio::test]
async fn a_stale_stats_block_is_dim() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    assert_eq!(
        common::style_of(&app.frame(120, 14).unwrap(), "⚠").fg,
        Some(p.yellow),
        "the counters are live and one of them warns"
    );
    app.inject_stale_stats();
    let buf = app.frame(120, 14).unwrap();
    assert_eq!(common::style_of(&buf, "⚠").fg, Some(p.overlay1));
}

/// The character under a mid-string cursor has its colours reversed and is never replaced: a
/// block over it would hide it, and the cursor exists to show where you are in what you typed. A
/// text snapshot cannot see a style, so this is the only place the rule can be pinned.
#[tokio::test]
async fn the_prompt_reverses_the_character_under_a_mid_string_cursor() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    app.handle(Key::Char(':'));
    for c in "subnet".chars() {
        app.handle(Key::Char(c));
    }
    for _ in 0..3 {
        app.handle(Key::Left);
    }
    let (w, h) = (120u16, 21u16);
    let buf = app.frame(w, h).unwrap();
    // The prompt is the row above the status line.
    let y = h - 2;
    let row: String = (0..w)
        .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    assert!(row.starts_with(":subnet"), "nothing is hidden: {row:?}");
    // One cursor, one pair of colours: the mid-string cell is the block's colours inverted the
    // way `theme::selected_row()` inverts them, not the terminal's default under `REVERSED`.
    let p = nutsh_tui::theme::snapshot();
    let under = buf.cell((4, y)).expect("inside the buffer");
    assert_eq!(under.symbol(), "n");
    assert_eq!(under.bg, p.mauve, "the same mauve as the end-of-line block");
    assert_eq!(under.fg, p.base);
    let after = buf.cell((5, y)).expect("inside the buffer");
    assert_eq!(after.symbol(), "e");
    assert_ne!(after.bg, p.mauve, "and only that one cell");
}

/// A cursor never splits what the terminal draws as one cell. `e` followed by U+0301 is two
/// scalars and one grapheme; ratatui drops a zero-width grapheme that *begins* a span, so a
/// reversed cell covering only the `e` loses the accent from the frame entirely - which is
/// exactly what `cursor_spans`'s own doc says must never happen.
#[tokio::test]
async fn the_cursor_covers_a_whole_grapheme_cluster() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    app.handle(Key::Char(':'));
    for c in "ae\u{301}b".chars() {
        app.handle(Key::Char(c));
    }
    // `←` twice: past `b`, then past the whole `e´` rather than into it.
    app.handle(Key::Left);
    app.handle(Key::Left);
    let (w, h) = (120u16, 21u16);
    let buf = app.frame(w, h).unwrap();
    let y = h - 2;
    let row: String = (0..w)
        .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    assert!(
        row.starts_with(":ae\u{301}b"),
        "the accent survives the cursor: {row:?}"
    );
    // Column 0 is the `:` lead, so the cluster's lead cell is column 2.
    let under = buf.cell((2, y)).expect("inside the buffer");
    assert_eq!(under.bg, p.mauve, "and it is under the cursor: {row:?}");
    assert_eq!(under.fg, p.base);
}

/// The ghost is dim and what was typed is not. The whole meaning of the offer is carried by that
/// difference - on `:subn█et`, dim `et` says "press tab first" and text-coloured `et` would say
/// "press enter" - and a text snapshot cannot see it, so the two colours are asserted here as
/// *different*, not merely as present.
#[tokio::test]
async fn the_ghost_is_dimmer_than_what_was_typed() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    app.handle(Key::Char(':'));
    for c in "subn".chars() {
        app.handle(Key::Char(c));
    }
    let (w, h) = (120u16, 21u16);
    let buf = app.frame(w, h).unwrap();
    // The prompt is the row above the status line.
    let y = h - 2;
    let row: String = (0..w)
        .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    assert!(row.starts_with(":subn█et"), "{row:?}");
    // Column 0 is the `:` lead, so column 1 is the typed `s`, column 5 the block, and the
    // ghost starts at column 6.
    let typed = buf.cell((1, y)).expect("inside the buffer");
    assert_eq!(typed.symbol(), "s");
    assert_eq!(typed.fg, p.text, "what was typed");
    let ghost = buf.cell((6, y)).expect("inside the buffer");
    assert_eq!(ghost.symbol(), "e");
    assert_eq!(ghost.fg, p.overlay1, "what is being offered");
    assert_ne!(typed.fg, ghost.fg, "and the two are not the same colour");
}

/// The composed detail's tints, none of which a text snapshot can see: a section heading in the
/// table-header yellow, labels dim, values in the text colour. The whole layout rests on the
/// label/value difference - the eye runs down the boundary between a dim label and a lit value -
/// so drawing both in one colour would leave every text assertion green and the pane unreadable.
#[tokio::test]
async fn the_composed_detail_dims_its_labels_and_lights_its_values() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('y'));
    common::settle(&mut app).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(100, 23).unwrap();
    assert_eq!(
        common::style_of(&buf, "IDENTITY").fg,
        Some(p.yellow),
        "a section heading is the table-header style"
    );
    assert_eq!(
        common::style_of(&buf, "Description").fg,
        Some(p.overlay1),
        "a label is dim"
    );
    assert_eq!(
        common::style_of(&buf, "frontend").fg,
        Some(p.text),
        "and the value it labels is not"
    );
    assert_ne!(p.text, p.overlay1, "the two colours are not the same");
}

/// The menu's width, and the colour of its pane's top-left corner: the body starts under the
/// seven-row header and the menu is the first thing in it.
const MENU_WIDTH: u16 = 24;

fn menu_border(buf: &ratatui::buffer::Buffer) -> Option<ratatui::style::Color> {
    let corner = buf.cell((0u16, 7u16)).expect("the menu's top-left corner");
    assert_eq!(corner.symbol(), "╭", "{}", common::text_of(buf));
    Some(corner.fg)
}

/// A kind this Prism Central does not serve is drawn in the greyed colour the `Missing` items
/// use - the visible half of "greyed, not gone", and the half a text snapshot cannot assert.
#[tokio::test]
async fn an_unserved_kind_is_drawn_greyed_in_the_menu() {
    let pc = MockPc::builder()
        .unavailable_namespace("volumes")
        .start()
        .await;
    let mut app = app(&pc).await;
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    app.handle(Key::Char('2'));
    app.handle(Key::Char('l'));
    let buf = app.frame(120, 30).unwrap();
    // Within the menu's columns: the body draws the same words when that table is open.
    let style = common::style_of_within(&buf, 0..24, "Volume Groups");
    assert_eq!(style.fg, Some(p.overlay1));
}
