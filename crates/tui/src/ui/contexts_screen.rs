//! The Contexts screen.

use super::*;

/// One line of the contexts list. The header goes through it too, so the columns cannot drift
/// from their titles. `lead` is the three-character selection-and-current marker.
pub(super) fn context_line(lead: &str, cells: [&str; 6]) -> String {
    let [name, host, user, cluster, flags, password] = cells;
    // Every column but the last is cut to its own width: a long host name must not push the
    // user and the flags out from under their headings.
    format!(
        "{lead}{} {} {} {} {} {password}",
        field(name, 14),
        field(host, 24),
        field(user, 10),
        field(cluster, 14),
        field(flags, 18),
    )
}

/// How many lines of the message area the screen gives the last result.
pub(super) const MESSAGE_LINES: usize = 3;

/// The contexts, then a message area for the last thing that happened: a connect in flight, an
/// error, a removal, the confirmation of one.
///
/// The two are laid out rather than concatenated. A list longer than the screen would
/// otherwise push the message off the bottom, and `remove NAME? y/n` below the fold is a
/// question the user answers `y` to without ever seeing it.
pub(super) fn draw_contexts(app: &App, f: &mut Frame, body: Rect) {
    let s = &app.screen;
    let width = usize::from(body.width);
    let said = message_lines(s, width);
    // A blank line above what it says, and never more of the body than the list itself.
    let wanted = if said.is_empty() { 0 } else { said.len() + 1 };
    let height = u16::try_from(wanted)
        .unwrap_or(u16::MAX)
        .min(body.height.saturating_sub(1));
    let [list, message] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).areas(body);
    f.render_widget(Paragraph::new(context_lines(s, list, width)), list);
    let said: Vec<Line> = std::iter::once(Line::from(""))
        .chain(said.into_iter().map(Line::from))
        .collect();
    f.render_widget(Paragraph::new(said), message);
}

/// What the screen has to say under the rows: the removal it is waiting on, or the last
/// result and whatever reading the rows had to say.
pub(super) fn message_lines(s: &contexts::Screen, width: usize) -> Vec<String> {
    if let Some(name) = &s.confirm_remove {
        return vec![cut(&format!("remove {name}? y/n"), width)];
    }
    [s.message.as_deref(), s.list_error.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(str::lines)
        .take(MESSAGE_LINES)
        .map(|line| cut(line, width))
        .collect()
}

/// The heading and as many rows as `area` holds, framing the selection like the palette and
/// the picker do; the count of what is left below goes on the last line, like the `(N more)`
/// on a modal's border.
pub(super) fn context_lines(s: &contexts::Screen, area: Rect, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // "You have no contexts" and "your config file could not be read" are different answers,
    // and inviting the user to add a context is wrong advice for the second.
    if s.rows.is_empty() {
        if s.list_error.is_none() {
            lines.push(Line::from(
                "No contexts. Press a to add one, or run nutsh ctx add.",
            ));
        }
        return lines;
    }
    lines.push(
        Line::from(cut(
            &context_line(
                "   ",
                ["NAME", "HOST", "USER", "CLUSTER", "FLAGS", "PASSWORD"],
            ),
            width,
        ))
        .style(Style::default().add_modifier(Modifier::BOLD)),
    );
    // The heading, and the hint line whenever the list is longer than the area - reserved
    // whether or not anything is below the window, so scrolling does not move every row by
    // one when the last of them comes into view.
    let room = usize::from(area.height).saturating_sub(1);
    let scrolls = s.rows.len() > room;
    let rows = if scrolls {
        room.saturating_sub(1)
    } else {
        room
    };
    let (offset, window) = crate::palette::window(&s.rows, s.selected, rows);
    for (i, r) in window.iter().enumerate() {
        // The default port is implied; anything else is part of the address.
        let host = if r.port == DEFAULT_PORT {
            r.host.clone()
        } else {
            format!("{}:{}", r.host, r.port)
        };
        let mut flags = Vec::new();
        if r.insecure {
            flags.push("insecure");
        }
        if r.readonly {
            flags.push("read-only");
        }
        // Not `marker()`: this list carries two markers, the cursor and the `*` of the
        // current context, and they have to sit in fixed columns of their own.
        let lead = format!(
            "{}{} ",
            if offset + i == s.selected { ">" } else { " " },
            if r.current { "*" } else { " " }
        );
        lines.push(Line::from(cut(
            &context_line(
                &lead,
                [
                    &r.name,
                    &host,
                    &r.username,
                    r.cluster.as_deref().unwrap_or("-"),
                    &flags.join(" "),
                    if r.has_password { "stored" } else { "-" },
                ],
            ),
            width,
        )));
    }
    if scrolls {
        let more = s.rows.len() - (offset + window.len());
        lines.push(Line::from(if more > 0 {
            format!("({more} more)")
        } else {
            String::new()
        }));
    }
    lines
}
