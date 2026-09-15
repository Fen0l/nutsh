//! The seven-line header and its folded form.

use super::*;

/// The stats block's width, and the hint grid's: two 13-cell columns with a 2-cell gap.
pub(super) const STATS_WIDTH: u16 = 26;

pub(super) const HINTS_WIDTH: u16 = 28;

/// The narrowest info half worth having. Below `HINTS_WIDTH + INFO_MIN + 2` - a 70-cell box,
/// which with the 26-cell stats block beside it is a 96-column frame - the grid is dropped
/// and its hints move to the prompt line.
pub(super) const INFO_MIN: u16 = 40;

pub(super) fn draw_header(app: &App, f: &mut Frame, area: Rect, body: u16) {
    let [boxed, stats] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(STATS_WIDTH)]).areas(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(Span::styled(" nutsh ", theme::title()));
    let inner = block.inner(boxed);
    f.render_widget(block, boxed);
    // The Contexts screen goes without a grid rather than advertise `⏎ drill` where `enter`
    // connects; the table's and the page's own grids are in `hint_lines`.
    let grid = !app.on_contexts_screen() && inner.width >= HINTS_WIDTH + INFO_MIN;
    let [info, hints] = Layout::horizontal([
        Constraint::Min(INFO_MIN),
        Constraint::Length(if grid { HINTS_WIDTH } else { 0 }),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(info_lines(app, usize::from(info.width), body)),
        info,
    );
    if grid {
        f.render_widget(Paragraph::new(hint_lines(app)), hints);
    }
    f.render_widget(
        Paragraph::new(stats_lines(app)).alignment(Alignment::Right),
        stats,
    );
    // The seventh line of the block, when the block is tall enough to draw it.
    if stats.height >= 7 {
        app.hits.borrow_mut().version = Some(Rect {
            y: stats.y + 6,
            height: 1,
            ..stats
        });
    }
}

/// The whole header on one line: what this is, what the body is showing, which cluster the
/// session is scoped to, who is connected, whether it can change anything, and how fresh the
/// rows are.
///
/// The four facts the full box carries that nothing else on the frame does - context, cluster
/// scope, read-only, freshness - plus the breadcrumb, because a reader who cannot see the
/// `Kind:` field has no other way to tell which of two similar tables is open. What is dropped
/// with the six rows is the hint grid, which the prompt line and the `?` overlay both name in
/// full, and the stats block, which is a Prism Central's summary rather than this session's.
///
/// The freshness readout is right-aligned in its own cell, as it is on the status line, so it
/// sits at the same column in both heights and a reader's eye does not have to look for it.
pub(super) fn draw_folded_header(app: &App, f: &mut Frame, area: Rect, body: u16) {
    let [left, sync] =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(SYNC_WIDTH)]).areas(area);
    const LEAD: &str = " nutsh ";
    /// What is never dropped to make room, only cut off by the backstop below.
    const KEEP: u8 = 3;
    // Everything after the elastic part, built first so the elastic part can be told what it
    // has left, and each tagged with what it is worth when the line cannot hold all of it.
    //
    // The count and the breadcrumb are on the table's own title bar as well, so they are the
    // cheap ones; the cluster scope, the context and `[read-only]` are on nothing else on the
    // frame, and `[read-only]` governs what a keystroke can do. Dropping by worth rather than
    // by position is what keeps the reading order the same at every width: the line loses
    // facts from the left, never from the end.
    let mut tail: Vec<(u8, Span<'static>)> = Vec::new();
    let head = match &app.live {
        // Without a session the same line says what there is to say, as `no_session_lines`
        // does: there is no context, no cluster and nothing polling.
        None => {
            tail.push((
                KEEP,
                Span::styled("  no session", Style::default().fg(theme::mauve())),
            ));
            app.config_path.clone().unwrap_or_else(|| "-".to_string())
        }
        Some(live) => {
            let s = &live.session;
            // Bracketed and labelled, where the box has a column of labels to lean on: a bare
            // `3` beside a bare `-` on one line is two facts a reader has to guess at, and a
            // frame this app is read from is a frame somebody has to be able to read cold.
            tail.push((
                0,
                Span::styled(format!("  [{}]", count_text(app, body)), theme::dim()),
            ));
            tail.push((1, Span::styled("  cluster:", theme::dim())));
            tail.push((
                1,
                Span::styled(
                    s.cluster
                        .as_ref()
                        .map_or("<all>", |c| c.name.as_str())
                        .to_string(),
                    Style::default().fg(theme::green()),
                ),
            ));
            tail.push((2, Span::styled("  ctx:", theme::dim())));
            tail.push((
                2,
                Span::styled(
                    s.context.clone().unwrap_or_else(|| "-".to_string()),
                    Style::default().fg(theme::mauve()),
                ),
            ));
            if s.readonly {
                tail.push((
                    KEEP,
                    Span::styled("  [read-only]", Style::default().fg(theme::red())),
                ));
            }
            if s.insecure {
                tail.push((
                    KEEP,
                    Span::styled("  [insecure]", Style::default().fg(theme::peach())),
                ));
            }
            breadcrumb(app)
        }
    };
    // The elastic part: the breadcrumb, or the config path when there is no session. Cut from
    // the **left**, because the tail of either is the half that says which one it is -
    // `…Storage › VMs`, `…/nutsh/config.toml`.
    let room = usize::from(left.width);
    let budget = room.saturating_sub(LEAD.width());
    // Cheapest first, until what is left fits. The breadcrumb has already given up everything
    // it has by then, since it takes only what the survivors leave.
    let width_of = |tail: &[(u8, Span<'static>)]| -> usize {
        tail.iter().map(|(_, s)| s.content.width()).sum()
    };
    for worth in 0..KEEP {
        if width_of(&tail) <= budget {
            break;
        }
        tail.retain(|(w, _)| *w > worth);
    }
    let mut spans = vec![
        Span::styled(LEAD, theme::title()),
        Span::styled(
            elide_left(&head, budget.saturating_sub(width_of(&tail))),
            Style::default().fg(if app.live.is_some() {
                theme::peach()
            } else {
                theme::sapphire()
            }),
        ),
    ];
    // The backstop, for a frame too narrow even for what `KEEP` marks: whole spans, and the
    // first that does not fit ends the line. One row is the whole point, so nothing may wrap.
    let mut used: usize = spans.iter().map(|s| s.content.width()).sum();
    for (_, span) in tail {
        let width = span.content.width();
        if used + width > room {
            break;
        }
        used += width;
        spans.push(span);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), left);
    let (label, colour) = sync_indicator(app);
    right_label(f, sync, &label, colour);
}

/// `{label:<12}` dim, then the value in the field's own colour.
pub(super) fn field_line(label: &str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<12}"), theme::dim())];
    spans.extend(value);
    Line::from(spans)
}

pub(super) fn info_lines(app: &App, width: usize, body: u16) -> Vec<Line<'static>> {
    let Some(live) = &app.live else {
        return no_session_lines(app, width);
    };
    let s = &live.session;
    // The default port is implied; anything else (the mock's ephemeral port, a proxy) is not.
    let endpoint = if s.port == DEFAULT_PORT {
        s.host.clone()
    } else {
        format!("{}:{}", s.host, s.port)
    };
    let mut context = vec![Span::styled(
        s.context.clone().unwrap_or_else(|| "-".to_string()),
        Style::default().fg(theme::mauve()),
    )];
    if s.readonly {
        context.push(Span::styled(
            "  [read-only]",
            Style::default().fg(theme::red()),
        ));
    }
    if s.insecure {
        context.push(Span::styled(
            "  [insecure]",
            Style::default().fg(theme::peach()),
        ));
    }
    let mut pc = vec![Span::styled(
        endpoint,
        Style::default().fg(theme::sapphire()),
    )];
    if let Some(v) = &s.pc_version {
        pc.push(Span::styled(
            format!("  {v}"),
            Style::default().fg(theme::sapphire()),
        ));
    }
    vec![
        field_line("Context:", context),
        field_line("PC:", pc),
        field_line(
            "Cluster:",
            vec![Span::styled(
                s.cluster
                    .as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "<all>".to_string()),
                Style::default().fg(theme::green()),
            )],
        ),
        field_line(
            "Kind:",
            vec![Span::styled(
                cut(&breadcrumb(app), width.saturating_sub(12)),
                Style::default().fg(theme::peach()),
            )],
        ),
        field_line(
            "Count:",
            vec![Span::styled(
                count_text(app, body),
                Style::default().fg(theme::text()),
            )],
        ),
    ]
}

/// Without a session the same five fields say what there is to say: which file the contexts
/// came from, and how many there are. An empty screen looks the same whether the config is
/// missing, elsewhere, or genuinely empty; the path says which.
pub(super) fn no_session_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    vec![
        field_line(
            "Context:",
            vec![Span::styled(
                "no session",
                Style::default().fg(theme::mauve()),
            )],
        ),
        field_line(
            "Config:",
            vec![Span::styled(
                elide_left(
                    app.config_path.as_deref().unwrap_or("-"),
                    width.saturating_sub(12),
                ),
                Style::default().fg(theme::sapphire()),
            )],
        ),
        field_line(
            "Cluster:",
            vec![Span::styled("-", Style::default().fg(theme::green()))],
        ),
        field_line(
            "Kind:",
            vec![Span::styled(
                "Contexts",
                Style::default().fg(theme::peach()),
            )],
        ),
        field_line(
            "Count:",
            vec![Span::styled(
                contexts_count(app),
                Style::default().fg(theme::text()),
            )],
        ),
    ]
}

/// `N contexts`: what `Count:` says while the Contexts screen is on screen.
pub(super) fn contexts_count(app: &App) -> String {
    let n = app.screen.rows.len();
    let plural = if n == 1 { "context" } else { "contexts" };
    format!("{n} {plural}")
}

/// `Group › Item › child`: the menu's path to what is open, then the drill-down.
pub(super) fn breadcrumb(app: &App) -> String {
    let live = match &app.live {
        Some(live) if !app.on_contexts_screen() => live,
        _ => return "Contexts".to_string(),
    };
    let mut parts = Vec::new();
    if let Some(target) = &app.sidebar.open
        && let Some((group, label)) = nutsh_catalog::nav_path(target)
    {
        parts.push(group.to_string());
        parts.push(label.to_string());
    }
    for (i, view) in live.stack.iter().enumerate() {
        // A page names itself through the menu item above it, and its panes are named in
        // their own blocks; only the tables of the stack add to the path.
        let View::Table(view) = view else { continue };
        if let Some(parent) = &view.parent_name {
            parts.push(parent.clone());
        }
        // The first table is already named by the menu item above it - unless that item opened
        // a page, and this table came out of one of its panes.
        if i > 0 || parts.len() < 2 || view.parent_name.is_some() {
            parts.push(view.key.kind.display.to_string());
        }
    }
    parts.join(" › ")
}

/// `body` is the height of the area below the header, which is what decides how many of a
/// page's panes the frame has room for.
pub(super) fn count_text(app: &App, body: u16) -> String {
    if app.on_contexts_screen() {
        return contexts_count(app);
    }
    // A page has as many row counts as it has panes, and each pane's title already carries
    // its own; what the field can say for the screen as a whole is how many there are - and,
    // on a frame too short for the whole grid, how many of them were drawn. That is the one
    // place the notice belongs: written over the body it would land on the first pane's
    // border and title.
    if let Some(page) = app.page() {
        let rows = page_rows(page.def, body);
        let drawn = page
            .panes
            .iter()
            .filter(|p| usize::from(p.def.row) < rows)
            .count();
        let total = page.panes.len();
        return if drawn == total {
            format!("{total} panes")
        } else {
            format!("{drawn} of {total} panes")
        };
    }
    let (Some(live), Some(view)) = (&app.live, app.view()) else {
        return "-".to_string();
    };
    count(live.store.table(&view.key), view.query())
}

/// Five rows of two 13-cell cells: `{key:>2} {label:<10}`, key sky bold, label dim.
///
/// Per view, because the keys are: a page sorts and widens nothing, and a table has no panes.
/// Every pair names a key the view actually binds, and the grid is full: five rows is what the
/// seven-line header box holds beside five `info_lines`, and two 13-cell cells is what 28 cells
/// hold. So `a actions` - the key to the twenty-five actions a VM has, and the one thing here
/// no other surface said - cost `^b menu` its cell. `^b:menu` is on the prompt line at every
/// width and toggles a pane that is on screen; `a` was on nothing and opens what is not.
///
/// §11.2's grid offers `a actions` on a *page* too, and it is there now that `handle_page`
/// binds it: the whole action pipeline reads `App::cursor_table`, which answers for a focused
/// pane as it does for a table view. It cost the page's grid `^b menu` for the reason it cost
/// the table's - `^b:menu` is on the prompt line at every width and toggles a pane that is on
/// screen, where `a` was on nothing and opens what is not.
pub(super) fn hint_lines(app: &App) -> Vec<Line<'static>> {
    const TABLE: [[(&str, &str); 2]; 5] = [
        [("⏎", "drill"), ("y", "detail")],
        [("S", "sort"), ("w", "wide")],
        [("←→", "columns"), ("^r", "refresh")],
        [("^t", "every"), ("^x", "stop")],
        [("a", "actions"), ("^c", "quit")],
    ];
    const PAGE: [[(&str, &str); 2]; 5] = [
        [("⏎", "detail"), ("O", "open table")],
        [("⇥", "pane"), ("y", "detail")],
        [("jk", "move"), ("^r", "refresh")],
        [("^t", "every"), ("^x", "stop")],
        [("a", "actions"), ("^c", "quit")],
    ];
    let hints = if app.page().is_some() { &PAGE } else { &TABLE };
    hints
        .iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (i, (key, label)) in row.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(
                    format!("{key:>2} "),
                    Style::default()
                        .fg(theme::sky())
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(format!("{label:<10}"), theme::dim()));
            }
            Line::from(spans)
        })
        .collect()
}

/// Seven lines, right-aligned in 26 cells, the first and the sixth blank: the four counters
/// sit level with `Context:`..`Kind:` and the version line with the box's bottom border, as
/// in §11.1. Labels dim, numbers text, `on` green and `off` red, `⚠`/`✖` yellow and red and
/// both dim at zero so a quiet Prism Central looks quiet, `▶ N running` peach above zero, and
/// the version line dim with the PC version sapphire. The version line is `v0.0.2-beta1 ·
/// pc.7.6`: this program's version and the Prism Central's, in that order, and neither named
/// because the box beside them says which is which.
pub(super) fn stats_lines<'a>(app: &'a App) -> Vec<Line<'a>> {
    use nutsh_core::stats::{Count, Stats, show};
    // Assembled at compile time: the one label that is not a literal is the same on every frame.
    // No program name: the box's own title, two cells to the left on the same frame, already
    // reads `nutsh`. Naming it twice on one line was affordable while the version was five
    // characters and is not at eleven, and this block is twenty-six cells wide - the six that
    // buys are what keeps the Prism Central's version from being clipped off the end.
    const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"), " · ");
    let empty = Stats::default();
    let s = app.live.as_ref().map_or(&empty, |l| l.store.stats());
    let link = if app.mouse_enabled() {
        Modifier::UNDERLINED
    } else {
        Modifier::empty()
    };
    // A stale block is dim throughout, so a screen of numbers never looks live when it is not.
    let plain = Style::default().fg(if s.stale {
        theme::overlay1()
    } else {
        theme::text()
    });
    let tinted = |colour| Style::default().fg(if s.stale { theme::overlay1() } else { colour });
    let label = |t: &'static str| Span::styled(t, theme::dim());
    let value = |t: String, style: Style| Span::styled(t, style);
    let warn = |c: Count, colour| {
        Style::default().fg(if c.unwrap_or(0) == 0 || s.stale {
            theme::overlay1()
        } else {
            colour
        })
    };
    vec![
        Line::from(""),
        Line::from(vec![
            label("clusters "),
            value(show(s.clusters), plain),
            label("  hosts "),
            value(show(s.hosts), plain),
        ]),
        Line::from(vec![
            label("vms "),
            value(show(s.vms), plain),
            label("  on/off "),
            value(show(s.vms_on), tinted(theme::green())),
            label("/"),
            value(show(s.vms_off()), tinted(theme::red())),
        ]),
        Line::from(vec![
            label("alerts "),
            value(
                format!("⚠ {}", show(s.alerts_warning)),
                warn(s.alerts_warning, theme::yellow()),
            ),
            label("  "),
            value(
                format!("✖ {}", show(s.alerts_critical)),
                warn(s.alerts_critical, theme::red()),
            ),
        ]),
        Line::from(vec![
            label("tasks "),
            value(
                format!("▶ {} running", show(s.tasks_running)),
                warn(s.tasks_running, theme::peach()),
            ),
        ]),
        Line::from(""),
        // Underlined while the mouse is captured: the line is the one click that opens the
        // settings screen, and nothing else on the header is a link.
        Line::from(vec![
            Span::styled(VERSION, theme::dim().add_modifier(link)),
            Span::styled(
                app.live
                    .as_ref()
                    .and_then(|l| l.session.pc_version.as_deref())
                    .unwrap_or("-"),
                Style::default().fg(theme::sapphire()).add_modifier(link),
            ),
        ]),
    ]
}
