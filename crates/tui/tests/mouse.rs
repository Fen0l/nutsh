//! The mouse, with no terminal involved: `App::frame` draws through the same path the real
//! loop does, so it populates `App::hits`, and every assertion below is about state.

mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::{Focus, Mode};
use nutsh_tui::{App, Key, Mouse, MouseKind};

async fn app(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    app
}

fn click(col: u16, row: u16) -> Mouse {
    Mouse {
        kind: MouseKind::Click,
        col,
        row,
    }
}

fn wheel(kind: MouseKind, col: u16, row: u16) -> Mouse {
    Mouse { kind, col, row }
}

/// The frame `App::frame` drew is the layout the click is resolved against, so a test asks the
/// hit map where a row is rather than counting border cells by hand.
fn row_y(app: &App, index: usize) -> u16 {
    let hits = app.hits_for_test();
    let t = hits.tables.first().expect("a table was drawn");
    t.rows.y + u16::try_from(index - t.offset).expect("on screen")
}

/// How far `←`/`→` have scrolled the table view's columns.
fn col_offset(app: &App) -> usize {
    app.live
        .as_ref()
        .and_then(|live| live.table())
        .expect("a table view")
        .col_offset
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_selects_the_row_under_it() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.frame(120, 30).unwrap();
    let y = row_y(&app, 2);
    app.mouse(click(30, y));
    assert_eq!(app.selected_name(), Some("db-01"));
    assert!(app.dirty, "the frame changed");
}

/// Selection only: `enter` still drills, so a mis-aimed click cannot navigate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_never_drills() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.frame(120, 30).unwrap();
    let y = row_y(&app, 1);
    app.mouse(click(30, y));
    app.mouse(click(30, y));
    assert!(
        app.snapshot(120, 30).unwrap().contains("Virtual Machines"),
        "still the VM table"
    );
}

/// A click below the last row moves focus and selects nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_in_the_empty_space_selects_nothing() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Esc); // focus the menu, so the click has something to move
    app.frame(120, 30).unwrap();
    let below = {
        let hits = app.hits_for_test();
        let t = hits.tables.first().unwrap();
        t.rows.y + t.rows.height - 1
    };
    app.mouse(click(30, below));
    assert_eq!(
        app.selected_name(),
        Some("web-01"),
        "the selection did not move"
    );
    assert_eq!(app.focus, Focus::Body, "the focus did");
}

/// A click on a header sorts by that column; a second flips it. `S` keeps cycling as it does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_on_a_header_sorts_and_flips() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.frame(120, 30).unwrap();
    let (x, y) = {
        let hits = app.hits_for_test();
        let t = hits.tables.first().unwrap();
        // The second drawn column: POWER.
        let (x, _, _) = t.columns[1];
        (x, t.header.y)
    };
    app.mouse(click(x, y));
    assert_eq!(app.sort_for_test(), Some((1, false)), "ascending by POWER");
    app.mouse(click(x, y));
    assert_eq!(
        app.sort_for_test(),
        Some((1, true)),
        "the second click flips it"
    );
    // And a third column is a fresh ascending sort, not a third state.
    let other = {
        let hits = app.hits_for_test();
        let t = hits.tables.first().unwrap();
        t.columns[2].0
    };
    app.mouse(click(other, y));
    assert_eq!(app.sort_for_test(), Some((2, false)));
}

/// After `→` has scrolled the columns, the header under the pointer is not the one at that
/// index of the full list - which is what `shown[i]` in the recorded span is for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_sorts_by_the_column_that_is_actually_there() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Right);
    app.frame(120, 30).unwrap();
    let (x, y, wanted) = {
        let hits = app.hits_for_test();
        let t = hits.tables.first().unwrap();
        let (x, _, index) = t.columns[1];
        (x, t.header.y, index)
    };
    assert!(
        wanted > 1,
        "the scroll moved the second drawn column past index 1"
    );
    app.mouse(click(x, y));
    assert_eq!(app.sort_for_test(), Some((wanted, false)));
}

/// The wheel is a three-at-a-time `j`/`k`, and it does **not** change focus: scrolling a pane
/// you are only glancing at should not steal the keyboard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wheel_moves_the_selection_without_taking_focus() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.inject_rows(50);
    app.handle(Key::Esc); // the menu has focus
    app.frame(120, 30).unwrap();
    let y = row_y(&app, 0);
    app.mouse(wheel(MouseKind::WheelDown, 30, y));
    assert_eq!(app.selected_name(), Some("vm-003"));
    app.mouse(wheel(MouseKind::WheelUp, 30, y));
    assert_eq!(app.selected_name(), Some("vm-000"));
    assert_eq!(
        app.focus,
        Focus::Sidebar,
        "the wheel did not steal the keyboard"
    );
}

/// Nothing moved, nothing to redraw: a wheel at the top of a list already at its first row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wheel_that_changes_nothing_leaves_dirty_false() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.frame(120, 30).unwrap();
    let y = row_y(&app, 0);
    app.clear_dirty_for_test();
    app.mouse(wheel(MouseKind::WheelUp, 30, y));
    assert!(!app.dirty);
}

/// A click on a menu item opens it - the same code path `enter` takes, so a group toggles and
/// an item opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_in_the_menu_opens_the_item() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.frame(120, 30).unwrap();
    let (x, y) = {
        let hits = app.hits_for_test();
        let s = hits.sidebar.as_ref().expect("the menu was drawn");
        // The first row is the `Dashboard` group header; clicking it expands the group.
        (s.rows.x + 3, s.rows.y)
    };
    app.mouse(click(x, y));
    assert_eq!(app.focus, Focus::Sidebar);
    let expanded = app.snapshot(120, 30).unwrap();
    assert!(expanded.contains("▾ Dashboard"), "{expanded}");
}

/// A stray click must not throw away a half-typed `:` command, and it must not reach the table
/// under the modal either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_outside_a_modal_changes_nothing() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    for c in "vm".chars() {
        app.handle(Key::Char(c));
    }
    app.frame(120, 30).unwrap();
    let before = app.snapshot(120, 30).unwrap();
    app.clear_dirty_for_test();
    app.mouse(click(2, 29));
    assert!(!app.dirty);
    assert_eq!(app.snapshot(120, 30).unwrap(), before);
}

/// Inside it, the click moves the modal's selection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_inside_a_modal_moves_its_selection() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    app.frame(120, 30).unwrap();
    let (x, y) = {
        let hits = app.hits_for_test();
        let m = hits.modal.as_ref().expect("the palette was drawn");
        (m.rows.x + 3, m.rows.y + 2)
    };
    app.mouse(click(x, y));
    assert_eq!(app.palette_selected_for_test(), 2);
}

/// A click on a pane's block focuses **that** pane - the one the hit names, not the one whose
/// place in the hit map it happens to occupy.
///
/// The Dashboard's four panes are Clusters, Unresolved alerts, Running tasks and VMs; the
/// second reads `monitoring`, which this Prism Central does not serve, so `draw_pane` takes its
/// early return and draws the reason where a table would be. The hit map is then
/// `[pane 0, pane 2, pane 3]` and `tables[1]` is pane **2**: an index into `Hits::tables` is
/// not an index into `PageView::panes`, which is why the hit carries `pane`.
///
/// Nothing exotic about the trigger, either: `body_note` answers `Some` for any settled empty
/// pane, so a Dashboard with nothing running reaches the same branch on `Running tasks`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_on_a_panes_block_focuses_the_pane_the_hit_names() {
    let pc = MockPc::builder()
        .unavailable_namespace("monitoring")
        .start()
        .await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "dashboard",
        common::config(),
    )
    .unwrap();
    common::settle_page(&mut app).await;
    app.frame(140, 40).unwrap();
    assert_eq!(
        app.focused_pane_for_test(),
        Some(0),
        "the page opens on its first pane"
    );
    let (x, y) = {
        let hits = app.hits_for_test();
        assert_eq!(
            hits.tables.len(),
            3,
            "four panes, and the unserved one drew no table"
        );
        let second = &hits.tables[1];
        assert_eq!(
            second.pane,
            Some(2),
            "the second table drawn is the third pane"
        );
        (second.block.x, second.block.y) // the top-left corner of its border
    };
    app.mouse(click(x, y));
    assert_eq!(app.focused_pane_for_test(), Some(2));

    // And a horizontal wheel over a pane is nothing: a pane is drawn at `col_offset: 0` and
    // has no column scroll for one to move.
    app.clear_dirty_for_test();
    app.mouse(wheel(MouseKind::WheelRight, x + 2, y + 2));
    assert!(!app.dirty, "a pane has no columns to scroll");
}

/// `WheelLeft`/`WheelRight` over the table view is the `←`/`→` column scroll, clamped the same
/// way: a wheel left at the first column is not a negative offset.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_horizontal_wheel_is_the_column_scroll() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.frame(120, 30).unwrap();
    let y = row_y(&app, 0);
    assert_eq!(col_offset(&app), 0);
    app.mouse(wheel(MouseKind::WheelRight, 30, y));
    assert_eq!(col_offset(&app), 1, "one column, as `→` does");
    app.mouse(wheel(MouseKind::WheelLeft, 30, y));
    assert_eq!(col_offset(&app), 0, "and back, as `←` does");
    app.mouse(wheel(MouseKind::WheelLeft, 30, y));
    assert_eq!(col_offset(&app), 0, "clamped at the first column");
}

/// `ctrl-o` this session only; `:mouse` writes the answer back. Both are toggles, and the
/// event loop reconciles the terminal to whatever the app says on its next iteration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_o_toggles_capture_for_this_session() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    assert!(app.mouse_enabled(), "the config default is on");
    app.handle(Key::Ctrl('o'));
    assert!(!app.mouse_enabled());
    // And the pointer stops doing anything, because the loop stops sending events; the app
    // itself is unchanged, which is what makes the toggle one line in the loop.
    app.handle(Key::Ctrl('o'));
    assert!(app.mouse_enabled());
}

/// The header's version line is the one thing on the header a click does something to: the
/// settings screen opens, and once it is up the click that opened it is not read as a row.
#[tokio::test]
async fn a_click_on_the_version_line_opens_settings() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.frame(120, 30).unwrap();
    let (x, y) = {
        let hits = app.hits_for_test();
        let r = hits
            .version
            .expect("the full header draws the version line");
        // Right-aligned text: the last cells are the ones that hold it.
        (r.x + r.width - 2, r.y)
    };
    app.mouse(click(x, y));
    assert_eq!(app.mode, Mode::Settings);
    app.mouse(click(x, y));
    assert_eq!(
        app.mode,
        Mode::Settings,
        "inert over a modal, like the rest of the header"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);

    // Folded, the header has no version line and the same cells are the table's.
    app.set_header(nutsh_tui::app::Header::Compact);
    app.frame(120, 30).unwrap();
    assert!(app.hits_for_test().version.is_none(), "nothing to click on");
}
