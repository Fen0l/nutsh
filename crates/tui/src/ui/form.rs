//! Action forms, their fields and the reference picker.

use super::*;

/// The add form, or the login field, which is the same box with only its password row. Sized
/// to what it holds: a one-field prompt in a box for eight would read as a broken form.
pub(super) fn draw_form(app: &App, f: &mut Frame, body: Rect) {
    let s = &app.screen;
    /// What the fields are laid out for; a longer line widens the box rather than being cut.
    const WIDTH: u16 = 60;
    // `▸ ` marks the field the keys go to, and the cursor says where the next one lands: a
    // checkbox takes none - space toggles it - so it gets the marker alone.
    let row = |active: bool, cursor: Option<usize>, label: &str, value: String| -> Line<'static> {
        let marker = if active {
            Span::styled("▸ ", Style::default().fg(theme::peach()))
        } else {
            Span::raw("  ")
        };
        let mut spans = vec![marker, Span::styled(format!("{label:<10} "), theme::dim())];
        spans.extend(owned_cursor_spans(
            &value,
            cursor,
            Style::default().fg(theme::text()),
            theme::peach(),
        ));
        Line::from(spans)
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    let (title, error) = match &s.prompt {
        Some(Prompt::Login(login)) => {
            lines.push(row(
                true,
                Some(login.cursor()),
                "password",
                contexts::mask(&login.password),
            ));
            (
                format!("password for {}", login.name),
                login.error.as_deref(),
            )
        }
        Some(Prompt::Add(form)) => {
            for field in contexts::FIELDS {
                let active = field == form.current();
                // `Form::cursor` answers for the field the keys go to, and `None` for a
                // checkbox - the same split as `Form::text_mut`.
                let cursor = active.then(|| form.cursor()).flatten();
                lines.push(row(active, cursor, field.label(), form.value(field)));
            }
            ("add context".to_string(), form.error.as_deref())
        }
        None => return,
    };
    // While the connect runs the box stays open with everything typed in it, so it says what
    // it is waiting for where the answer will go: the pending message dim, an error red.
    let note = if s.pending {
        s.message
            .as_deref()
            .map(|m| Span::styled(m.to_string(), theme::dim()))
    } else {
        error.map(|e| Span::styled(e.to_string(), Style::default().fg(theme::red())))
    };
    if let Some(note) = note {
        lines.push(Line::from(""));
        lines.push(Line::from(note));
    }
    boxed_lines(f, body, &title, lines, WIDTH);
}

/// A centred, rounded box around `lines`: wide enough for the widest of them and never
/// narrower than `min_width`, because a sentence cut in half explains nothing. A body narrower
/// or shorter than that clamps the box, and the paragraph clips at the border.
///
/// The Contexts prompt and an action's form are the same box around different rows; the rows
/// are what differs between them, and this is not.
pub(super) fn boxed_lines(
    f: &mut Frame,
    body: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    min_width: u16,
) {
    let widest = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = u16::try_from(widest.saturating_add(2))
        .unwrap_or(u16::MAX)
        .max(min_width);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered_rect_with_min(0, 0, width, height, body);
    clear_region(f, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::peach()))
                .title(Span::styled(format!(" {title} "), theme::title())),
        ),
        area,
    );
}

/// The chosen action's form: one row per visible field, the cursor on the selected one, and
/// what `build_body` refused with under a blank line. The reference picker, when one is open,
/// is drawn after it - it is a list of rows, and the form is what it writes into.
///
/// Not [`draw_form`], which draws the Contexts screen's fixed struct: these rows come from the
/// catalog, and their types decide how a value is shown.
pub(super) fn draw_fields(app: &App, f: &mut Frame, body: Rect) {
    let Some(form) = &app.form else {
        return;
    };
    /// What the rows are laid out for; a longer line widens the box rather than being cut.
    const WIDTH: u16 = 60;
    /// The label column. Wide enough for the longest curated label, and a longer one pushes
    /// its own value right rather than being cut.
    const LABEL: usize = 16;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, field) in form.fields.iter().enumerate() {
        let active = i == form.selected;
        let raw = form.value(field.name);
        // A checkbox shows its state rather than the word `true`, and takes no cursor: space
        // toggles it. Everything else is typed, so the cursor says where the next key lands.
        let (value, cursor) = match field.ty {
            FieldType::Bool => {
                let box_ = if raw == "true" { "[x]" } else { "[ ]" };
                (box_.to_string(), None)
            }
            FieldType::Enum(_) => (raw.to_string(), None),
            _ => (raw.to_string(), active.then(|| form.cursor())),
        };
        // A required field says so where it is read, not in a legend at the bottom.
        let name = nutsh_core::actions::label_of(field);
        let label = if field.required {
            format!("{name} *")
        } else {
            name.to_string()
        };
        let mut spans = vec![
            if active {
                Span::styled("▸ ", Style::default().fg(theme::peach()))
            } else {
                Span::raw("  ")
            },
            Span::styled(format!("{label:<LABEL$} "), theme::dim()),
        ];
        spans.extend(owned_cursor_spans(
            &value,
            cursor,
            Style::default().fg(theme::text()),
            theme::peach(),
        ));
        lines.push(Line::from(spans));
    }
    // `create` on a VM has thirty-eight visible fields and twenty catalog forms have fifteen
    // or more, while the box is clamped to the body: unwindowed, `tab` walks the cursor onto
    // rows that were never drawn, and the refusal underneath goes off the bottom with them.
    // The error keeps its two lines whatever is showing, so it is always the last thing in the
    // box rather than line n+2 of a column that no longer reaches n.
    let error = form.error.as_deref().map(|e| {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                e.to_string(),
                Style::default().fg(theme::red()),
            )),
        ]
    });
    let reserved = 2 + error.as_ref().map_or(0, Vec::len);
    let rows = usize::from(body.height).saturating_sub(reserved);
    let (_, shown) = crate::palette::window(&lines, form.selected, rows);
    let mut lines = shown.to_vec();
    lines.extend(error.unwrap_or_default());
    boxed_lines(f, body, &form.title, lines, WIDTH);
    draw_ref_picker(form, f, body);
}

/// The rows of the kind a `Reference` field names, beside the form it writes into. Empty while
/// the one-shot list is still in flight, which is what `loading…` says.
pub(super) fn draw_ref_picker(form: &crate::form::Form, f: &mut Frame, body: Rect) {
    let Some(p) = form.picker.as_ref() else {
        return;
    };
    // The corner, not the centre: the form is centred, and a box over the fields would hide
    // the very rows the choice is being made for.
    let area = popup_area(body, p.entries.len().max(1));
    clear_region(f, area);
    let rows = usize::from(area.height.saturating_sub(2));
    let (offset, window) = p.window(rows);
    let width = usize::from(area.width);
    let mut items: Vec<ListItem> = window
        .iter()
        .map(|choice| {
            tagged_row(
                &choice.name,
                Style::default().fg(theme::text()),
                Span::styled(elide_left(&choice.ext_id, 16), theme::dim()),
                width,
            )
        })
        .collect();
    if items.is_empty() {
        // Three states, not two: still walking the pages, done and empty, or done and failed.
        // A kind that could not be read saying "no rows" is a lie the user would act on.
        let note = match (form.loading, form.listing_error.as_ref()) {
            (true, _) => "loading…".to_string(),
            (false, Some(e)) => format!("list failed: {}", e.text()),
            (false, None) => "no rows".to_string(),
        };
        items.push(ListItem::new(Span::styled(note, theme::dim())));
    }
    // The placeholder is not a row `enter` could take, so nothing is selected on it.
    let selected = (!p.entries.is_empty()).then(|| p.selected - offset);
    let mut state = ListState::default().with_selected(selected);
    render_framed_list(
        f,
        area,
        items,
        Line::from(vec![
            Span::styled(" choose ", theme::title()),
            Span::styled(
                format!("{} ", cut(p.label, 28)),
                Style::default().fg(theme::peach()),
            ),
        ]),
        Scroll::of(offset, window.len(), p.entries.len()),
        &mut state,
    );
}
