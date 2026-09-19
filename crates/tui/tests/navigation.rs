mod common;

use nutsh_core::scheduler::Msg;
use nutsh_core::store::Failure;
use nutsh_mockpc::MockPc;
use nutsh_prism::PrismError;
use nutsh_tui::app::{Mode, resolve_kind};
use nutsh_tui::palette::Entry;
use nutsh_tui::{App, Key, table};

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
    app
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle(Key::Char(c));
    }
}

#[tokio::test]
async fn palette_ranks_kinds_and_greys_the_unreachable() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app.handle(Key::Char(':'));
    assert_eq!(app.mode, Mode::Command);
    type_str(&mut app, "vm");
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("palette_vm", app.snapshot(120, 21).unwrap()));
    let frame = app.snapshot(120, 21).unwrap();
    // The id is the row's tag, elided from the left to its sixteen cells.
    assert!(frame.contains("…m.ahv.config.Vm"), "{frame}");

    app.handle(Key::Esc);
    app.handle(Key::Char(':'));
    type_str(&mut app, "cluster");
    app.handle(Key::Enter);
    common::settle(&mut app).await;
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "clustermgmt.config.Cluster"
    );

    // A child kind: the palette greys it, and enter jumps to its parent's table. (`nics`
    // resolves only to the VM's NICs; `disks` would rank the cluster-level Disks kind first.)
    app.handle(Key::Char(':'));
    type_str(&mut app, "nics");
    let frame = app.snapshot(120, 21).unwrap();
    assert!(frame.contains("open from Virtual Machines"), "{frame}");
    // The reason survives the narrowest terminal worth supporting; the id is what gets cut.
    let narrow = app.snapshot(80, 21).unwrap();
    assert!(narrow.contains("open from Virtual Machines"), "{narrow}");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "vmm.ahv.config.Vm",
        "the parent opened"
    );
    assert_eq!(
        app.status.as_deref(),
        Some("open NICs from a row of Virtual Machines: pick one and press enter")
    );

    // A kind that needs a parameter: enter only explains.
    app.handle(Key::Char(':'));
    type_str(&mut app, "entity-descriptor");
    app.handle(Key::Enter);
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "vmm.ahv.config.Vm",
        "nothing opened"
    );
    assert_eq!(app.status.as_deref(), Some("needs a parameter"));
}

#[tokio::test]
async fn palette_opens_a_kind_as_a_new_root() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "cluster");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "clustermgmt.config.Cluster"
    );
    common::settle(&mut app).await;
    assert_eq!(app.selected_name(), Some("lab-cluster"));
}

#[tokio::test]
async fn enter_drills_into_children_and_esc_pops() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Picker);
    let frame = app.snapshot(120, 23).unwrap();
    assert!(frame.contains("Disks") && frame.contains("NICs"), "{frame}");
    // Pick "Disks": type to filter, then enter. What is typed shows on the prompt line, where
    // the `type to filter` hint was.
    type_str(&mut app, "disk");
    let frame = app.snapshot(120, 23).unwrap();
    assert!(
        frame.lines().rev().nth(1).unwrap().starts_with(" disk█"),
        "{frame}"
    );
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Table);
    let view = app.view().unwrap();
    assert_eq!(view.key.kind.id, "vmm.ahv.config.Disk");
    assert_eq!(
        view.key.parents,
        vec!["3d0c4a2e-1b8f-4c1a-9e2f-000000000001".to_string()]
    );
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("vm_disks", app.snapshot(120, 19).unwrap()));
    let frame = app.snapshot(120, 19).unwrap();
    // The menu's path to what is open leads the drill-down, exactly as the design's §9 spells
    // this example.
    assert!(
        frame.contains("Compute & Storage › VMs › web-01 › Disks"),
        "{frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.view().unwrap().key.kind.id, "vmm.ahv.config.Vm");
    assert_eq!(app.selected_name(), Some("web-01"));
}

/// `y` opens the composed summary; `Y` and `J` show the wire document and return to it. A field
/// the summary omits is found in the raw view, and that is the answer - there is no per-field
/// search and no "show all fields" toggle, because three ways to see one document is two too
/// many.
#[tokio::test]
async fn detail_opens_composed_and_y_or_j_shows_the_wire_document() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    app.handle(Key::Char('y'));
    assert_eq!(app.mode, Mode::Detail);
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    let settings = common::settings(&pc);
    let frame = app.snapshot(100, 23).unwrap();
    assert!(frame.contains("Virtual Machines · web-01"), "{frame}");
    assert!(
        frame.contains("IDENTITY"),
        "composed, not serialised: {frame}"
    );
    assert!(!frame.contains("$objectType"), "{frame}");
    assert!(
        frame.contains("● live"),
        "the document follows a subscription of its own: {frame}"
    );
    settings.bind(|| insta::assert_snapshot!("detail_composed", app.snapshot(100, 23).unwrap()));

    app.handle(Key::Char('Y'));
    let frame = app.snapshot(100, 23).unwrap();
    assert!(frame.contains("name: web-01"), "{frame}");
    settings.bind(|| insta::assert_snapshot!("detail_yaml", app.snapshot(100, 23).unwrap()));
    app.handle(Key::Char('Y'));
    assert!(
        app.snapshot(100, 23).unwrap().contains("IDENTITY"),
        "Y returns to the composed view"
    );

    app.handle(Key::Char('J'));
    let frame = app.snapshot(100, 23).unwrap();
    assert!(frame.contains("\"name\": \"web-01\""), "{frame}");
    settings.bind(|| insta::assert_snapshot!("detail_json", app.snapshot(100, 23).unwrap()));
    app.handle(Key::Char('J'));
    let frame = app.snapshot(100, 23).unwrap();
    assert!(
        frame.contains("IDENTITY"),
        "J returns to the composed view: {frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
}

#[tokio::test]
async fn sort_wide_and_help() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('S'));
    assert_eq!(app.view().unwrap().sort, Some((0, false)));
    assert_eq!(
        app.selected_name(),
        Some("db-01"),
        "sorted by NAME ascending"
    );
    app.handle(Key::Char('S'));
    assert_eq!(app.view().unwrap().sort, Some((0, true)));
    assert_eq!(app.selected_name(), Some("web-02"));
    app.handle(Key::Char('w'));
    assert!(app.view().unwrap().wide);
    let frame = app.snapshot(200, 19).unwrap();
    assert!(
        frame.contains("CLUSTER"),
        "wide shows more columns: {frame}"
    );
    app.handle(Key::Char('?'));
    assert_eq!(app.mode, Mode::Help);
    let frame = app.snapshot(100, 27).unwrap();
    assert!(
        frame.contains("^r refresh  ^x stop  ^t schedule"),
        "{frame}"
    );
    // The overlay is the one place a user reads the palette's keys, and its longest line is the
    // one that would clip: `Paragraph` truncates rather than wraps, so assert the whole run.
    assert!(
        frame.contains("⇥ complete · ↑↓ pick · ^p ^n history · ^w delete word"),
        "the palette's key line, whole: {frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
}

#[tokio::test]
async fn palette_quit_command() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "q");
    app.handle(Key::Enter);
    assert!(app.should_quit());
}

#[tokio::test]
async fn the_palette_scrolls_to_keep_the_selection_visible() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "v");
    let entries = app.palette.as_ref().unwrap().entries.len();
    assert!(entries > 9, "a list longer than the box: {entries}");
    for _ in 0..12 {
        app.handle(Key::Down);
    }
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("palette_scrolled", app.snapshot(120, 21).unwrap()));
    let frame = app.snapshot(120, 21).unwrap();
    let p = app.palette.as_ref().unwrap();
    let Some(Entry::Kind { kind, .. }) = p.current() else {
        panic!("the thirteenth match for `v` is a kind: {:?}", p.current());
    };
    assert!(
        frame.contains(&format!("▌ {}", kind.display)),
        "the selection is inside the box: {frame}"
    );
    // Eight rows of entries: a twelve-row body less the table's two border rows, which the
    // popup keeps inside, less its own two.
    assert_eq!(p.selected, 12);
    let (offset, window) = p.window(8);
    assert!(
        offset <= p.selected && p.selected < offset + window.len(),
        "selected {} not in {offset}..{}",
        p.selected,
        offset + window.len()
    );
}

#[tokio::test]
async fn the_picker_scrolls_to_keep_the_selection_visible() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "cluster");
    app.handle(Key::Enter);
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Picker);
    let children = app.picker.as_ref().unwrap().entries.len();
    assert!(children > 9, "a list longer than the box: {children}");
    for _ in 0..11 {
        app.handle(Key::Down);
    }
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("picker_scrolled", app.snapshot(120, 23).unwrap()));
    let frame = app.snapshot(120, 23).unwrap();
    let p = app.picker.as_ref().unwrap();
    let chosen = match p.current().expect("the selection is on a child") {
        nutsh_tui::picker::Entry::Child { kind, .. } => kind.display,
        other => panic!("the selection is on a child, not {other:?}"),
    };
    assert!(
        frame.contains(&format!("▌ {chosen}")),
        "the selection is inside the box: {frame}"
    );
    // Eight rows of entries: the box is its ten-row minimum in a fourteen-row body. Row 0 is
    // the `related` caption, which the cursor opens below and never lands on, so eleven steps
    // from row 1 end on row 12 - the last of the cluster's twelve children, and one short of
    // the `actions` caption the cursor would skip over.
    assert_eq!(p.selected, 12);
    let (offset, window) = p.window(8);
    assert!(
        offset <= p.selected && p.selected < offset + window.len(),
        "selected {} not in {offset}..{}",
        p.selected,
        offset + window.len()
    );
}

#[tokio::test]
async fn help_draws_over_the_view_it_was_opened_from() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('y'));
    common::settle(&mut app).await;
    app.handle(Key::Char('?'));
    assert_eq!(app.mode, Mode::Help);
    let frame = app.snapshot(100, 27).unwrap();
    assert!(
        frame.contains("Virtual Machines · web-01"),
        "the detail is still behind the help: {frame}"
    );
    assert!(
        frame.contains("^r refresh  ^x stop  ^t schedule"),
        "{frame}"
    );
    assert!(
        frame.contains("esc close"),
        "the help has a footer: {frame}"
    );
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Detail, "back to where help was opened");
}

/// The twenty-five VM actions sit behind `a`, which must be advertised somewhere: the header's
/// hint grid, the prompt line, or the `?`
/// overlay. "There are features like power on but we don't know how to use them" is the shape
/// that omission took as a bug report, so what this asserts is the *screen* - a key named only
/// by a constant nobody draws is not advertised.
#[tokio::test]
async fn every_surface_names_the_action_key() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    // Wide enough for the hint grid, which `draw_header` drops below 96 columns.
    let wide = app.snapshot(120, 20).unwrap();
    assert!(
        wide.contains("a actions"),
        "the hint grid names `a`: {wide}"
    );
    assert!(
        wide.contains("a:actions"),
        "the prompt line names `a`: {wide}"
    );
    // The narrowest frame worth supporting has no grid, so the prompt line carries it alone -
    // and must still fit, since the line is clipped rather than wrapped.
    let narrow = app.snapshot(80, 20).unwrap();
    assert!(narrow.contains("a:actions"), "{narrow}");
    assert!(
        narrow.lines().all(|l| l.chars().count() <= 80),
        "nothing overflows the narrowest frame: {narrow}"
    );

    app.handle(Key::Char('?'));
    let help = app.snapshot(100, 27).unwrap();
    // `a` opens the menu; `space` is how the menu comes to mean more than one row. A user who
    // cannot find the first cannot find the second either.
    assert!(help.contains("a            actions"), "{help}");
    assert!(help.contains("space        mark"), "{help}");
}

/// The `?` overlay is at zero slack, by construction: fourteen lines in a fourteen-row
/// interior, its widest line sixty-eight cells in a sixty-eight-cell one. `Paragraph` clips
/// rather than wraps and says nothing when it does, and the box cannot grow - a taller one
/// covers the title of the view it was opened over, which
/// `help_draws_over_the_view_it_was_opened_from` asserts on. So the fit is measured off the
/// drawn frame rather than eyeballed in the source.
#[tokio::test]
async fn the_help_overlay_fills_its_box_and_no_more() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('?'));
    let frame = app.snapshot(100, 27).unwrap();
    let rows: Vec<Vec<char>> = frame.lines().map(|l| l.chars().collect()).collect();
    // The only square-cornered block on the frame: every other one is `BorderType::Rounded`.
    let top = rows
        .iter()
        .position(|r| r.contains(&'\u{250c}'))
        .unwrap_or_else(|| panic!("the help box is drawn: {frame}"));
    let x = rows[top].iter().position(|&c| c == '\u{250c}').unwrap();
    let width = rows[top][x..]
        .iter()
        .position(|&c| c == '\u{2510}')
        .unwrap()
        + 1;
    let bottom = (top + 1..rows.len())
        .find(|&i| rows[i].get(x) == Some(&'\u{2514}'))
        .unwrap_or_else(|| panic!("the help box closes on screen: {frame}"));
    assert_eq!(width, 70, "the box is seventy cells wide: {frame}");
    assert_eq!(bottom - top - 1, 14, "fourteen lines of interior: {frame}");
    // The fourteenth line reached the screen, so the box was not clamped by a short body and
    // the text did not run out early. (A *fifteenth* line would be clipped in silence and no
    // frame could show it; `ui::tests::the_help_text_fits_its_box` is what catches that, and
    // this is the half that catches the box shrinking under it.)
    let last: String = rows[bottom - 1][x + 1..x + width - 1].iter().collect();
    assert!(
        last.trim_end().ends_with("quit"),
        "the last line of the help is on screen: {frame}"
    );
    // The widest line, whole. It is exactly the interior's sixty-eight cells, so its last
    // word is the one a sixty-ninth cell anywhere would have cost.
    assert!(
        frame.contains("mouse        click a row, a menu item or a header; the wheel scrolls"),
        "the widest line is not clipped: {frame}"
    );
}

#[tokio::test]
async fn a_detail_error_stays_in_the_pane() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('y'));
    common::settle(&mut app).await;
    let detail = app.detail.as_ref().expect("the pane is open");
    // An entity pane, so it has both: only a payload pane is over no table and polls nothing.
    let (sub, key, ext_id) = (
        detail.sub.expect("an entity pane polls"),
        detail.key.clone().expect("an entity pane has a table"),
        detail.ext_id.clone(),
    );
    let generation = app.live.as_ref().unwrap().store.table(&key).generation + 1;
    app.apply(Msg::Error {
        sub,
        key: key.clone(),
        generation,
        // One entity that has gone, not an endpoint that is not there.
        error: Failure::of(key.kind, &PrismError::NotFound("gone".into()), false),
    });

    let text = detail_text(&app);
    assert!(text.starts_with("error: not found (HTTP 404)"), "{text}");
    assert!(
        !text.contains("gone"),
        "and never the far end's own words: {text}"
    );
    assert!(
        text.contains("Name web-01"),
        "the entity is still there: {text}"
    );
    assert_eq!(
        app.live.as_ref().unwrap().store.table(&key).error,
        None,
        "one missing entity does not fail the whole table"
    );
    // The status line carries the pane's error the way it carries a table's, in the pane's
    // own words and not the server's.
    let frame = app.snapshot(100, 23).unwrap();
    let last = frame.lines().last().unwrap();
    assert!(
        last.contains("not found (HTTP 404)") && last.contains("○ syncing"),
        "{frame}"
    );

    // The next poll of the entity clears it.
    let entity = app
        .live
        .as_ref()
        .unwrap()
        .store
        .table(&key)
        .rows
        .get(&ext_id)
        .expect("the row the detail follows")
        .clone();
    app.apply(Msg::Entity {
        sub,
        key,
        generation: generation + 1,
        entity,
    });
    assert_eq!(app.detail.as_ref().unwrap().error, None);
}

/// The frames this file snapshots, and the pane's interior in one of them: the sidebar takes
/// [`MENU_WIDTH`] and the block's two borders the rest, which is one column of fields.
const FRAME_WIDTH: u16 = 100;
const MENU_WIDTH: u16 = 24;
const DETAIL_WIDTH: u16 = FRAME_WIDTH - MENU_WIDTH - 2;

/// The lines the detail pane would draw for the entity it follows, as one string. The error
/// lives above the body rather than inside the document, so only the lines carry both.
fn detail_text(app: &App) -> String {
    let live = app.live.as_ref().expect("connected");
    let detail = app.detail.as_ref().expect("the pane is open");
    let key = detail.key.as_ref().expect("an entity pane has a table");
    let lines = detail.lines(nutsh_tui::detail::BodyView {
        entity: live.store.table(key).rows.get(&detail.ext_id),
        names: live.store.names(),
        now: app.now,
        width: DETAIL_WIDTH,
        actions: &app.detail_actions(),
        extra: &app.detail_extra(),
    });
    common::text_of_lines(&lines)
}

#[tokio::test]
async fn opening_a_new_root_closes_the_detail_and_its_subscription() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('y'));
    let sub = app
        .detail
        .as_ref()
        .expect("the pane is open")
        .sub
        .expect("an entity pane polls");
    assert!(app.live.as_ref().unwrap().scheduler.is_live(sub));
    app.open_root(resolve_kind("cluster").unwrap());
    assert!(
        app.detail.is_none(),
        "the pane closed with the view under it"
    );
    assert!(
        !app.live.as_ref().unwrap().scheduler.is_live(sub),
        "its subscription went with it"
    );
    assert_eq!(app.mode, Mode::Table);
}

#[tokio::test]
async fn cancelling_the_picker_leaves_no_subscription() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Picker);
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
    assert!(app.picker.is_none());
    let live = app.live.as_ref().unwrap();
    assert_eq!(live.stack.len(), 1);
    // The root table's list is the one subscription; anything the picker pushed would be a
    // second.
    assert_eq!(live.scheduler.live_count(), 1);
}

#[tokio::test]
async fn sort_cycles_through_the_columns_and_back() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let columns = table::columns(app.view().unwrap().key.kind, false).len();
    for column in 0..columns {
        app.handle(Key::Char('S'));
        assert_eq!(app.view().unwrap().sort, Some((column, false)));
        app.handle(Key::Char('S'));
        assert_eq!(app.view().unwrap().sort, Some((column, true)));
    }
    app.handle(Key::Char('S'));
    assert_eq!(app.view().unwrap().sort, None, "back to the store's order");
}

#[tokio::test]
async fn wide_shows_every_column_and_back() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let kind = app.view().unwrap().key.kind;
    assert_eq!(table::columns(kind, false).len(), table::DEFAULT_COLUMNS);
    app.handle(Key::Char('w'));
    assert!(app.view().unwrap().wide);
    // VMs are curated, so "every column" is the curated list, not the schema's.
    assert_eq!(
        table::columns(kind, true).len(),
        table::WIDE_COLUMNS.min(kind.columns.len())
    );
    app.handle(Key::Char('w'));
    assert!(!app.view().unwrap().wide);
}

/// `esc` at the root pops nothing: the view, the stack and the cursor are where they were. It
/// is not a no-op - it moves the focus to the menu, which `tests/sidebar.rs` asserts.
#[tokio::test]
async fn esc_at_the_root_pops_nothing() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
    assert_eq!(app.live.as_ref().unwrap().stack.len(), 1);
    assert_eq!(app.view().unwrap().key.kind.id, "vmm.ahv.config.Vm");
    assert_eq!(app.selected_name(), Some("web-01"));
}

#[tokio::test]
async fn a_palette_row_the_session_cannot_serve_says_so() {
    // A namespace that answered at no version: the one refusal left that costs no request. A
    // kind merely newer than its namespace's pin is *attempted* now, and the server's 404 is
    // what greys it - the menu's business, not the palette's.
    let pc = MockPc::builder()
        .unavailable_namespace("volumes")
        .start()
        .await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "volume-group");
    let frame = app.snapshot(120, 21).unwrap();
    let reason = "namespace not served";
    // The popup's row names the kind and carries the reason as its tag: the name is what a
    // greyed row must keep, and the status line says the reason again once `enter` is pressed.
    let row = frame
        .lines()
        .find(|l| l.contains(reason))
        .unwrap_or_else(|| panic!("no row carries the reason: {frame}"));
    assert!(row.contains("Volume Groups"), "the label survives: {frame}");
    app.handle(Key::Enter);
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "vmm.ahv.config.Vm",
        "nothing opened"
    );
    assert_eq!(app.status.as_deref(), Some(reason));
}

/// The popup sits at the bottom left of the body, is 46 cells wide, and tags every row with
/// what it is: `cmd` for a command, the id for a reachable kind, the reason for a greyed one.
#[tokio::test]
async fn the_suggestion_popup_tags_every_row() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "vm");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(frame.contains("kinds & commands"), "{frame}");
    // The prompt line carries what was typed, with a cursor.
    assert!(
        frame.lines().rev().nth(1).unwrap().starts_with(":vm█"),
        "{frame}"
    );

    // That a greyed row's tag is its reason is this popup's business; which rows are greyed and
    // where they rank is the catalog's. Asked of `nics`, whose every row is greyed, rather than
    // of `vm`, where the first greyed row is the twelfth - near enough the bottom of the box
    // that a promotion in the ranking would read here as a geometry bug.
    app.handle(Key::Esc);
    app.handle(Key::Char(':'));
    type_str(&mut app, "nics");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(
        frame.contains("open from Virtual Machines"),
        "greyed rows keep their reason: {frame}"
    );

    app.handle(Key::Esc);
    app.handle(Key::Char(':'));
    type_str(&mut app, "q");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(frame.contains(":quit"), "commands are prefixed: {frame}");
    assert!(frame.contains("cmd"), "{frame}");

    // Nothing matches: the box stays, and says so, rather than vanishing under the prompt.
    app.handle(Key::Esc);
    app.handle(Key::Char(':'));
    type_str(&mut app, "zzzz");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(frame.contains("no matches"), "{frame}");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Command, "enter has nothing to take");
}

/// `:skin ` and `:ctx ` complete their argument in the same popup: one widget, four argument
/// sets.
#[tokio::test]
async fn the_popup_completes_command_arguments() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    // `enter` changes the process-global skin, so this test holds the guard like a style test.
    let _skin = common::pin_skin();
    app.handle(Key::Char(':'));
    type_str(&mut app, "skin gruv");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(frame.contains("gruvbox-dark"), "{frame}");
    assert!(frame.contains("gruvbox-light"), "{frame}");
    assert!(!frame.contains("catppuccin-mocha"), "filtered: {frame}");
    assert!(
        frame.contains(" skins "),
        "titled for what it lists: {frame}"
    );
    // §11 Case B: the ghost is the selected row's remainder, so the prompt and the list cannot
    // disagree - `↓` to `gruvbox-light` would make it `box-light` in the same keystroke.
    assert!(
        frame
            .lines()
            .rev()
            .nth(1)
            .unwrap()
            .starts_with(":skin gruv█box-dark"),
        "{frame}"
    );
    app.handle(Key::Enter);
    assert_eq!(app.status.as_deref(), Some("skin gruvbox-dark"));
    nutsh_tui::theme::apply_named("catppuccin-mocha").unwrap();
}

/// R4. One argument too many is a mistake to report, not one to drop. `:ctx` is the one
/// command that takes several - the first is the session, the rest are joined beside it - so
/// its refusal is about the name, not the count.
#[tokio::test]
async fn an_extra_argument_is_refused_not_dropped() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let before = app.view().unwrap().key.kind.id;
    app.handle(Key::Char(':'));
    type_str(&mut app, "ctx a b");
    app.handle(Key::Enter);
    assert_eq!(app.status.as_deref(), Some("no context named a"));
    assert_eq!(
        app.view().unwrap().key.kind.id,
        before,
        "nothing opened and nothing connected"
    );
    assert_eq!(app.mode, Mode::Table, "the palette closed onto its origin");

    // A command with no slots refuses any argument at all, from the same two facts.
    app.handle(Key::Char(':'));
    type_str(&mut app, "journal now");
    app.handle(Key::Enter);
    assert_eq!(app.status.as_deref(), Some(":journal takes no argument"));
    assert_eq!(
        app.mode,
        Mode::Table,
        "the journal did not open and the palette closed onto its origin"
    );
}

/// A nav item with nothing behind it is offered rather than hidden, and `enter` on it says why -
/// `menu.rs:9-11`'s argument, applied to the palette: a greyed row beats a vanished one, because
/// "there is no such thing in the v4 API" is an answer and an empty list is not.
#[tokio::test]
async fn a_missing_nav_item_is_offered_and_explains_itself() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let before = app.view().unwrap().key.kind.id;
    app.handle(Key::Char(':'));
    type_str(&mut app, "catalog");
    let frame = app.snapshot(100, 24).unwrap();
    assert!(frame.contains("Catalog Items"), "{frame}");
    assert!(
        frame.contains("not in the v4 API"),
        "greyed, with its note: {frame}"
    );
    app.handle(Key::Enter);
    assert_eq!(
        app.status.as_deref(),
        Some("Catalog Items: not in the v4 API")
    );
    assert_eq!(app.mode, Mode::Table, "the palette closed onto its origin");
    assert_eq!(app.view().unwrap().key.kind.id, before, "nothing opened");
}

/// The ghost on a frame. Styles are invisible in a text snapshot, so the ghost is asserted by
/// the characters *after* the block - which is exactly why the block is kept and the ghost drawn
/// after it. `tab` takes it, and `enter` opens the table without touching it at all.
#[tokio::test]
async fn the_prompt_offers_the_rest_of_the_best_match() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "subn");
    let settings = common::settings(&pc);
    // One render, asserted twice: the snapshot and the two `starts_with` cannot be looking at
    // different frames.
    let frame = app.snapshot(120, 21).unwrap();
    settings.bind(|| insta::assert_snapshot!("palette_ghost", &frame));
    assert!(
        frame.lines().rev().nth(1).unwrap().starts_with(":subn█et"),
        "{frame}"
    );
    assert!(frame.contains("▌ Subnets"), "the selected row: {frame}");

    app.handle(Key::Tab);
    let frame = app.snapshot(120, 21).unwrap();
    assert!(
        frame.lines().rev().nth(1).unwrap().starts_with(":subnet█"),
        "the ghost is taken and there is nothing left to offer: {frame}"
    );
    app.handle(Key::Enter);
    assert_eq!(app.view().unwrap().key.kind.id, "networking.config.Subnet");
}

/// What the palette accepted comes back on `ctrl-p`; what was cancelled never does. The test
/// config gives the app no cache directory, so this is the session-only history - which is
/// exactly what a user with `cache = false` or `--no-cache` gets, and the whole App path is the
/// same either way.
#[tokio::test]
async fn the_palette_remembers_what_it_accepted() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(':'));
    type_str(&mut app, "vm");
    app.handle(Key::Enter);

    // A cancelled line is not something the user committed to.
    app.handle(Key::Char(':'));
    type_str(&mut app, "never-typed-this");
    app.handle(Key::Esc);

    app.handle(Key::Char(':'));
    assert_eq!(app.palette.as_ref().unwrap().input.as_str(), "");
    app.handle(Key::Ctrl('p'));
    assert_eq!(app.palette.as_ref().unwrap().input.as_str(), "vm");
    let frame = app.snapshot(120, 21).unwrap();
    assert!(
        frame.lines().rev().nth(1).unwrap().starts_with(":vm█"),
        "the recalled line is on the prompt, cursor at its end: {frame}"
    );
    app.handle(Key::Ctrl('p'));
    assert_eq!(
        app.palette.as_ref().unwrap().input.as_str(),
        "vm",
        "one accepted line, and the oldest is the end"
    );
}

/// And with a cache directory it survives the process: the same state directory, a second
/// `App`, and `ctrl-p` still has the line. One switch governs both, so a run with no cache
/// writes no history file and the test above is the same feature without one.
#[tokio::test]
async fn the_history_survives_a_restart_when_the_run_caches() {
    let pc = MockPc::builder().start().await;
    let state = tempfile::tempdir().unwrap();
    let mut first = common::app_with_cache(&pc, state.path()).await;
    common::settle(&mut first).await;
    first.handle(Key::Char(':'));
    type_str(&mut first, "vm");
    first.handle(Key::Enter);
    drop(first);

    let mut second = common::app_with_cache(&pc, state.path()).await;
    common::settle(&mut second).await;
    second.handle(Key::Char(':'));
    second.handle(Key::Ctrl('p'));
    assert_eq!(second.palette.as_ref().unwrap().input.as_str(), "vm");
}
