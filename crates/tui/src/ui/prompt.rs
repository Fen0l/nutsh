//! The prompt and status lines at the foot of the frame.

use super::*;

/// The per-mode hint string in dim, or the active input. On a frame too narrow for the header's
/// hint grid this is where the long hint string goes.
pub(super) fn draw_prompt(app: &App, f: &mut Frame, area: Rect) {
    /// The typed text between its lead and its cursor, borrowed: it is redrawn every frame.
    /// `elided` prefixes a dim `…` for a line that scrolled off the left (design §8.4), and
    /// `ghost` is the rest of the best match, offered after the block.
    fn input<'a>(
        lead: &'static str,
        text: &'a str,
        cursor: usize,
        ghost: &'a str,
        elided: bool,
    ) -> Line<'a> {
        let mut spans = vec![Span::styled(
            lead,
            Style::default()
                .fg(theme::mauve())
                .add_modifier(Modifier::BOLD),
        )];
        if elided {
            spans.push(Span::styled("…", theme::dim()));
        }
        spans.extend(cursor_spans(
            text,
            Some(cursor),
            Style::default().fg(theme::text()),
            theme::mauve(),
        ));
        // After the block, never before it: the block marks the boundary between what was typed
        // and what is offered, which is also how a text snapshot asserts a ghost - `:subn█et`.
        if !ghost.is_empty() {
            spans.push(Span::styled(ghost, theme::dim()));
        }
        Line::from(spans)
    }
    // The picker's filter and the menu's are typed without a prefix, so they are shown without
    // one: the hint that says `type to filter` stays until the first letter, then makes way
    // for it.
    let typed = match app.mode {
        Mode::Picker => app.picker.as_ref().map(|p| p.input.as_str()),
        Mode::Menu => app.menu.as_ref().map(|m| m.input.as_str()),
        // The form itself types into its fields, where the text already shows; only the
        // reference picker over it has a filter with nowhere else to go.
        Mode::Fields => app
            .form
            .as_ref()
            .and_then(|f| f.picker.as_ref())
            .map(|p| p.input.as_str()),
        _ => None,
    }
    .filter(|text| !text.is_empty());
    let line = match (app.mode, typed) {
        (Mode::Command, _) => match app.palette.as_ref() {
            Some(p) => {
                // One cell for the `:` lead. The guard only exists for the palette: a filter
                // prompt is short, has never had one, and adding one would re-record its five
                // snapshots for a case that cannot happen.
                //
                // The room is measured on the *typed* text alone, so on a line long enough to
                // elide the ghost is what ratatui clips: the offer disappears rather than the
                // window scrolling to keep it. Whoever widens this guard owns that too.
                let (elided, start) = p.input.window(usize::from(area.width).saturating_sub(1));
                input(
                    ":",
                    &p.input.as_str()[start..],
                    p.input.cursor() - start,
                    p.ghost().unwrap_or(""),
                    elided,
                )
            }
            None => input(":", "", 0, "", false),
        },
        // The table's `/` term, with its own lead and a real cursor in it: unlike the three
        // filters below it is edited rather than typed and cleared, so a typo in the middle of
        // an address costs one `←` and not the whole line.
        (Mode::Table, _) if app.filtering() => match app.view().and_then(|v| v.filter.as_ref()) {
            Some(f) => input("/", f.input().as_str(), f.input().cursor(), "", false),
            None => input("/", "", 0, "", false),
        },
        // A filter is typed and cleared, never edited, so its block stays at the end, and it
        // completes nothing: the three filters rank no list to take a remainder from.
        (_, Some(text)) => input(" ", text, text.len(), "", false),
        // The body's keys give the line up to a task this session started, and take it back when
        // the last watch finishes. What a key has to say is on the status line under this one,
        // so neither ever hides the other.
        //
        // Only the body's: a modal's hint is the one place its keys are named - `y run` on a
        // confirm, `tab next` on a form, `esc close` on the journal - and a watch holds this
        // line for up to fifteen minutes, which is exactly when the next action is being
        // chained. The table's and the pane's keys are on the help overlay and in the header's
        // hint grid, so those two can lend the line out.
        (Mode::Table | Mode::Detail, _) => match task_line(app) {
            Some(text) => Line::from(Span::styled(
                format!(" {text}"),
                Style::default().fg(theme::subtext0()),
            )),
            None => Line::from(Span::styled(hint_text(app), theme::dim())),
        },
        _ => Line::from(Span::styled(hint_text(app), theme::dim())),
    };
    f.render_widget(Paragraph::new(line), area);
}

/// The progress of the task this session is following, or how many it is following when there
/// is more than one. `None` when none is running, which is when the hints come back.
pub(super) fn task_line(app: &App) -> Option<String> {
    let mut running = app.live.as_ref()?.tasks.running();
    let first = running.next()?;
    match running.count() {
        0 => Some(first.line()),
        rest => Some(format!("{} tasks running", rest + 1)),
    }
}

/// Each string carries its own leading space, so the prompt line is one borrowed span.
///
/// The line names the body's keys and does not change with the focus - the promise `tab:focus`
/// has always made. With the menu focused, `:`, `^b`, `S`, `w` and `?` still reach the body
/// through `Sidebar::Action::Passthrough`, `⇥` and `esc` come back to it, and `⏎` opens the
/// highlighted menu entry. So `esc:menu` names the gesture - `esc` is the way between the two
/// panes - rather than claiming which side you are standing on. Saying it per focus would
/// double every `Mode::Table` arm to change one word, and the menu's own keys - `/`, `h`/`l`,
/// the digits - are not on this line from either side.
pub(super) fn hint_text(app: &App) -> &'static str {
    match app.mode {
        // A page runs in `Mode::Table` and binds a smaller set: `tab` cycles its panes rather
        // than focusing the menu, and it neither sorts nor widens. Its `esc` is always the
        // menu: `App::open_page` drains the stack, so a page on top of it is its root - an
        // invariant that method debug-asserts, so the collapsed arm keeps a test-time guard.
        // `O:open table` gives up six cells to `a:actions`, which the grid spells in full
        // beside it: the line is 73 cells, under the 75 an 80-column terminal holds.
        Mode::Table if app.page().is_some() => {
            " :palette  ^b:menu  ⇥:pane  O:open  a:actions  ⏎:detail  esc:menu  ?:help"
        }
        // `y:detail` gives up its place to `esc`: the header's hint grid carries `y detail`,
        // and `esc` is the key whose meaning changes with where you are, so it is the one the
        // line has to spell out. `w:wide` gives up its place to `a:actions` for the same
        // reason the grid's `^b menu` did: both the grid and the `?` overlay name `w`, and
        // nothing named `a`. `S:sort` then gives up its place to `/:filter` on the same terms
        // and for a stronger reason: `S` is on the grid and in the `?` overlay, `/` was on
        // neither, and the line is the one surface a narrow terminal still has - which is
        // exactly where scrolling a thousand rows is least affordable. `tab` is spelled `⇥`,
        // as the page's line already spells it, to pay for the two cells. The line is 75
        // cells, which is what an 80-column terminal - the narrowest the tests render - holds;
        // a 76th would be clipped, not wrapped, and the key it took away would be `?:help`.
        Mode::Table if app.at_root() => {
            " :palette  ^b:menu  ⇥:focus  /:filter  a:actions  ⏎:drill  esc:menu  ?:help"
        }
        Mode::Table => {
            " :palette  ^b:menu  ⇥:focus  /:filter  a:actions  ⏎:drill  esc:back  ?:help"
        }
        // `PgUp/PgDn:page` gives up its place to `a:actions`: the pane now lists what can be
        // done to the row, and a list nothing says how to run is half a feature. The paging
        // keys keep their line on the `?` overlay, which is where they were learned anyway.
        Mode::Detail => " j/k:scroll  g/G:top/bottom  a:actions  Y:yaml  J:json  esc:back",
        Mode::Command => "",
        Mode::Skins => " enter apply  esc close",
        Mode::Ladder => " enter keep  esc close",
        Mode::Settings => " space:toggle  enter:choose  a:add  d:remove  esc:close",
        // The box lists both groups, so the one key it binds does both things: `enter` opens a
        // related kind and runs an action, and which it did is never a surprise.
        Mode::Picker => " type to filter  enter open or run  esc close",
        Mode::Menu => " type to filter  ↑↓ move  enter run  esc close",
        // A type-name confirm is the one that wants typing; a yes/no one must not advertise it.
        Mode::Confirm if app.confirm.as_ref().is_some_and(|c| c.expect.is_some()) => {
            " type what the prompt asks for  enter confirm  esc cancel"
        }
        Mode::Confirm => " y run  any other key cancels",
        // The picker over a reference field is a list, not a form: it binds its own keys.
        Mode::Fields if app.form.as_ref().is_some_and(|f| f.picker.is_some()) => {
            " type to filter  ↑↓ move  enter choose  esc back"
        }
        Mode::Fields => " tab next  space toggle  ←→ move/cycle  ^w word  enter submit  esc cancel",
        // Without a session there is no table behind the rows, so `esc` has nowhere to go and
        // promising it a way back is a lie the user only finds out by pressing it.
        Mode::Contexts if app.live.is_none() => {
            " enter connect  a add  l login  d remove  ^r reload  q quit"
        }
        Mode::Contexts => " enter connect  a add  l login  d remove  ^r reload  esc back  q quit",
        Mode::Form => " tab next  shift-tab previous  space toggle  enter submit  esc cancel",
        Mode::Help => " esc close",
        Mode::Journal => " j k g G scroll  esc close",
        Mode::Search => " j k move  enter open  esc close",
    }
}

/// `[Min(10), Length(METER_WIDTH), Length(SYNC_WIDTH)]`: the flash left, the meter, the sync
/// indicator right. The meter's cell is fixed so that a long flash cannot push it into the
/// indicator, and it is right-aligned inside it so the two readouts read as one block at the
/// right-hand end of the line.
///
/// The readout is taken *before* the layout so its cell can collapse to nothing when there is
/// none - a `--snapshot` run, an app with no session, any frame under 80 columns. Those frames
/// are exactly the ones that need the width elsewhere: the flash keeps every column it had
/// before the meter existed, so a connect error or a table's API error is cut where it always
/// was, and the sync indicator keeps its sixteen columns on a frame too narrow to seat three
/// cells (three cells need 52 columns to be drawn whole, two need 26).
pub(super) fn draw_status(app: &App, f: &mut Frame, area: Rect, folded: bool) {
    let readout = meter_readout(app, area.width);
    let [flash, meter, sync] = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(if readout.is_some() { METER_WIDTH } else { 0 }),
        Constraint::Length(if folded { 0 } else { SYNC_WIDTH }),
    ])
    .areas(area);
    let error = error_text(app);
    let (text, style) = match (app.status.as_deref(), error.as_deref()) {
        (Some(status), _) => (status, Style::default().fg(theme::subtext0())),
        (None, Some(error)) => (error, Style::default().fg(theme::red())),
        (None, None) => ("", theme::dim()),
    };
    let room = usize::from(flash.width).saturating_sub(1);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", style),
            Span::styled(cut(text, room), style),
        ])),
        flash,
    );
    if let Some((label, colour)) = readout {
        right_label(f, meter, &label, colour);
    }
    if !folded {
        let (label, colour) = sync_indicator(app);
        right_label(f, sync, &label, colour);
    }
}

/// `99.9 req/s · 100% cached` - the widest text there is, because `Meter::rate` is clamped at
/// 99.9 and a ratio at 100% - is 24 columns; the cell is that, plus the space that aligns it
/// with the sync indicator's own trailing space, plus one column of gutter so that even the
/// widest readout keeps a space between itself and the flash.
pub(super) const METER_WIDTH: u16 = 26;

/// The meter as the status line draws it, or `None` when there is nothing to say: no session
/// (nothing has been asked of anything, exactly as `sync_indicator` reads empty), a
/// `--snapshot` run, or a frame too narrow for even the rate.
///
/// Amber when the last ten seconds were paced - the local budget holding `acquire` back, or a
/// 429 the far end served, which drain the same bucket - and red when the pipe failed in the
/// same window. Red beats amber: a broken connection is the more useful of the two facts.
pub(super) fn meter_readout(app: &App, width: u16) -> Option<(String, Color)> {
    if app.one_frame_run {
        return None;
    }
    let live = app.live.as_ref()?;
    let meter = live
        .session
        .client
        .metrics()
        .snapshot(std::time::Instant::now());
    let text = nutsh_prism::meter_text(&meter, width)?;
    let colour = if meter.failed {
        theme::red()
    } else if meter.throttled {
        theme::peach()
    } else {
        theme::subtext0()
    };
    Some((format!("{text} "), colour))
}

/// What the flash reports in red when no status claims the line: the open detail's own error
/// first - one missing entity never marks its table failed, so the pane is the only place it
/// is recorded - else the current table's last error, or, on a page, the focused pane's.
///
/// A pane that already holds rows keeps drawing them when a later cycle fails, so without this
/// the failure would be nowhere on the frame: the pane's own block only says a reason instead
/// of rows while it has none.
///
/// Which is exactly the gate, and it was missing: a table or a pane the body is already
/// stating the failure for had it repeated down here, so one frame carried the same failure
/// twice. [`body_states_the_failure`] is the justification above, applied.
pub(super) fn error_text(app: &App) -> Option<String> {
    let live = app.live.as_ref()?;
    if let Some(failure) = app.detail.as_ref().and_then(|d| d.error.as_ref()) {
        return Some(failure.text());
    }
    let unstated = |key| {
        let t = live.store.table(key);
        (!body_states_the_failure(t)).then(|| t.error.as_ref().map(Failure::text))?
    };
    app.view()
        .and_then(|view| unstated(&view.key))
        .or_else(|| unstated(&app.page()?.focused()?.key))
}
