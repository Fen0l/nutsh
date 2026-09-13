//! Full-screen entity view: a composed, sectioned summary, or the wire document as YAML or JSON.

use std::time::SystemTime;

use nutsh_catalog::DETAIL_LABEL;
use nutsh_core::cell::{Names, Rendered};
use nutsh_core::detail::{self, Section};
use nutsh_core::scheduler::SubId;
use nutsh_core::store::{Failure, TableKey};
use nutsh_prism::Entity;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::key::Key;
use crate::theme;

/// What a key did, for the app to act on: the pane cannot reach the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Scrolled,
    Toggled,
    Help,
    Closed,
    Ignored,
}

/// Which of the three bodies the pane is showing.
///
/// `Detail::json: bool` was two states where there are three, and the third is the one this
/// phase exists for: a composed summary is what `y` opens, and the wire document is one
/// keystroke away in either notation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// The composed, sectioned summary. Reachable only over an entity of a known kind.
    Composed,
    Yaml,
    Json,
}

impl Body {
    /// The body a pane opens on, and the one `Y` and `J` return to.
    ///
    /// A payload pane - administration's request or response body - has no `TableKey`, so it has
    /// no `Kind`, so there is nothing to compose sections from and nothing to curate them
    /// against. Taking the key here rather than guarding every draw is what makes
    /// `Body::Composed` over a payload unrepresentable rather than merely unreached.
    pub(crate) fn default_for(key: Option<&TableKey>) -> Body {
        match key {
            Some(_) => Body::Composed,
            None => Body::Yaml,
        }
    }

    /// `Y` and `J`: to `raw`, or back to `home` when the pane is already showing `raw`.
    fn toggled(self, raw: Body, home: Body) -> Body {
        if self == raw { home } else { raw }
    }
}

/// What the pane needs from the app to draw itself. The pane cannot reach the store, so the app
/// hands it the entity, the shared name cache, the clock the ages are anchored to, and the
/// interior width the layout is computed against.
pub struct BodyView<'a> {
    pub entity: Option<&'a Entity>,
    pub names: &'a Names,
    pub now: SystemTime,
    /// The pane's interior width: the block's width less its two border columns.
    pub width: u16,
    /// What can be done to this row, greyed where it cannot: `App::action_rows`, the same list
    /// the `a` menu and the `⏎` picker draw. Empty on a payload pane, which is over no row.
    pub actions: &'a [crate::menu::Row],
}

/// A pane over one document: a row of a table, refreshed by a subscription of its own, or a
/// document an action answered with, which nothing refreshes.
///
/// The two are told apart by `key`: an entity pane has one and a payload pane has none, and
/// nothing but a payload pane sets `payload`. A caller that needs a row behind the pane -
/// anything reading the store, the composed body - asks for `key.is_some()` and leaves the
/// payload alone.
pub struct Detail {
    /// The table the entity belongs to; the single subscription stores its rows under it, so
    /// the pane reads the entity straight out of the table beneath it. `None` on a payload
    /// pane: it has no row anywhere, and everything it shows it carries.
    pub key: Option<TableKey>,
    pub ext_id: String,
    /// What refreshes the entity; `None` on a payload pane, so closing one unsubscribes
    /// nothing.
    pub sub: Option<SubId>,
    pub body: Body,
    pub scroll: u16,
    /// The single subscription's last error (a deleted entity, say), shown above the body.
    pub error: Option<Failure>,
    /// The document an action answered with, when this pane is over one rather than over a row.
    pub payload: Option<Value>,
    /// A payload pane's title; an entity pane titles itself from its kind and its row.
    pub heading: Option<String>,
}

impl Detail {
    /// `last` is the index of the last line the pane would draw; the app measures it, since only
    /// it can reach the entity. Scrolling stops there rather than running off the end.
    pub fn key(&mut self, key: Key, last: u16) -> Action {
        // A composed body's line count depends on the pane's width, so a resize can leave a
        // `scroll` the old width justified past the new end - and `k` would then walk back
        // through a dead range before the view moved at all. Clamping here, before any arm
        // reads or writes it, makes `scroll <= last` an invariant every arm inherits, and the
        // first key after a resize repairs the state.
        self.scroll = self.scroll.min(last);
        match key {
            Key::Esc => Action::Closed,
            Key::Char('Y') => {
                self.body = self.body.toggled(Body::Yaml, self.opens_on());
                self.scroll = 0;
                Action::Toggled
            }
            Key::Char('J') => {
                self.body = self.body.toggled(Body::Json, self.opens_on());
                self.scroll = 0;
                Action::Toggled
            }
            Key::Char('j') | Key::Down => {
                self.scroll = self.scroll.saturating_add(1).min(last);
                Action::Scrolled
            }
            Key::Char('k') | Key::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                Action::Scrolled
            }
            Key::PageDown => {
                self.scroll = self.scroll.saturating_add(20).min(last);
                Action::Scrolled
            }
            Key::PageUp => {
                self.scroll = self.scroll.saturating_sub(20);
                Action::Scrolled
            }
            Key::Char('g') | Key::Home => {
                self.scroll = 0;
                Action::Scrolled
            }
            Key::Char('G') | Key::End => {
                self.scroll = last;
                Action::Scrolled
            }
            Key::Char('?') => Action::Help,
            _ => Action::Ignored,
        }
    }

    /// A document an action answered with - a console token, a changed-regions report - with
    /// no subscription and no key behind it: there is nothing to poll and nothing to store.
    pub fn payload(title: String, value: Value) -> Detail {
        Detail {
            key: None,
            ext_id: String::new(),
            sub: None,
            body: Body::default_for(None),
            scroll: 0,
            error: None,
            payload: Some(value),
            heading: Some(title),
        }
    }

    /// The body this pane opens on, and the one `Y` and `J` come back to.
    fn opens_on(&self) -> Body {
        Body::default_for(self.key.as_ref())
    }

    /// Everything the pane draws: the single subscription's error above the body, then the body
    /// the current [`Body`] names.
    pub fn lines(&self, view: BodyView<'_>) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            // `Failure::text`, like every other surface: what the far end wrote is on the
            // `Failure` for `:journal`, and a detail pane is not a place to quote a gateway.
            lines.push(Line::from(Span::styled(
                format!("error: {}", error.text()),
                Style::default().fg(theme::red()),
            )));
            lines.push(Line::from(""));
        }
        match self.sections(&view) {
            // Every curated section came out empty over this row, or the document is not an
            // object to compose from. A bordered box with nothing in it says neither, so the
            // pane says what happened and falls through to the wire document.
            Some(sections) if sections.is_empty() => {
                lines.push(Line::from(Span::styled(
                    "nothing to compose from this document - the wire document follows",
                    theme::dim(),
                )));
                lines.push(Line::from(""));
                lines.extend(self.text_lines(view.entity));
            }
            Some(sections) => lines.extend(body_lines(sections, view.width)),
            // An entity pane whose row has not landed yet. A payload pane never waits: it
            // carries its document, and its `entity` is always `None`.
            None if view.entity.is_none() && self.payload.is_none() => {
                lines.push(Line::from(Span::styled("loading…", theme::dim())));
            }
            None => lines.extend(self.text_lines(view.entity)),
        }
        // Under the facts, not over them: the pane answers "what is this" first and "what can I
        // do to it" second. Only on the composed body - `Y` and `J` show the wire document, and
        // a section this app composed has no business inside one.
        if self.body == Body::Composed {
            lines.extend(action_lines(view.actions, view.width));
        }
        lines
    }

    /// The raw document as drawn lines, one per line of text.
    fn text_lines(&self, entity: Option<&Entity>) -> Vec<Line<'static>> {
        self.text(entity)
            .lines()
            .map(|l| Line::from(l.to_string()))
            .collect()
    }

    /// The sections to draw, when this pane is showing composed ones: [`Body::Composed`] over an
    /// entity pane whose row has landed. A payload pane has no key, so [`Body::default_for`]
    /// never opens it composed and `toggled` never sends it there.
    fn sections(&self, view: &BodyView<'_>) -> Option<Vec<Section>> {
        let (Body::Composed, Some(entity), Some(key)) = (self.body, view.entity, self.key.as_ref())
        else {
            return None;
        };
        Some(detail::compose(key.kind, entity, view.names, view.now))
    }

    /// `Virtual Machines · LAB-CLEAR03`, not `Virtual Machines LAB-CLEAR03 (c429f7a2-…)`. The
    /// extId moves into the Identity section - which is where a reader looks for an identifier -
    /// and the title gets 38 cells back for the name.
    pub fn title(&self, entity: Option<&Entity>) -> String {
        if let Some(heading) = &self.heading {
            return heading.clone();
        }
        // Neither a heading nor a key is the one state this type says cannot exist: `payload`
        // sets the heading, `open_detail` sets the key. It would title itself ` · …` rather
        // than say so, so a caller that breaks the invariant is caught in a test run instead.
        debug_assert!(self.key.is_some(), "an entity pane carries its table's key");
        let name = entity.map(|e| e.name.as_str()).unwrap_or("…");
        let display = self.key.as_ref().map_or("", |k| k.kind.display);
        format!("{display} · {name}")
    }

    /// The raw document as text. [`Body::Composed`] reaches here only when there was nothing to
    /// compose - it is composed, not serialised - and falls through to the YAML.
    fn text(&self, entity: Option<&Entity>) -> String {
        // A payload is the whole pane: it answers `J` the way an entity does, it never reaches
        // the store, and the `entity` a caller passes is not its business.
        if let Some(payload) = &self.payload {
            return if self.body == Body::Json {
                serde_json::to_string_pretty(payload).unwrap_or_default() + "\n"
            } else {
                nutsh_core::yaml::render(payload)
            };
        }
        match entity {
            None => "loading…\n".to_string(),
            Some(e) if self.body == Body::Json => {
                serde_json::to_string_pretty(&e.raw).unwrap_or_default() + "\n"
            }
            Some(e) => nutsh_core::yaml::render(&e.raw),
        }
    }

    /// The index of the last line the pane can scroll to. Measured off the lines the frame would
    /// draw, which is why the app records the pane's width: a composed body's line count depends
    /// on it, where a text body's does not.
    pub fn last_line(lines: &[Line<'static>]) -> u16 {
        u16::try_from(lines.len().saturating_sub(1)).unwrap_or(u16::MAX)
    }
}

/// Two columns at or above this interior width, one below. 92 gives a value column of 24 - a
/// `POWERED OFF`, an IPv4 and a `1.5 TiB` all fit whole - and below it the second column is
/// worth less than the spilling it would cause. 120 cells with the sidebar open is 94.
const TWO_COLUMN_MIN: u16 = 92;

/// The blank cells between a left field and the label of the right one. Spent out of the left
/// column's value budget, not out of the slack at the right margin: a value that filled its
/// column exactly would otherwise be drawn flush against the next label, and a reader could
/// tell neither where the value ended nor where the label began.
const GUTTER: usize = 2;

/// The composed sections as drawn lines, for a pane of interior width `width`.
///
/// A field is the label, one space, then the value; labels are right-aligned and dim, so label
/// and value sit adjacent and the eye runs down the boundary rather than down ragged whitespace.
/// A field whose label or value is wider than its column **spills**: it flushes the pair being
/// built and takes a whole line, which is how a 36-character UUID, a `hypervisor.fullName` and
/// an alert `message` stay whole instead of being cut.
///
/// The sections are consumed: `detail::compose` builds them for this frame and drops them, so
/// every value moves into its span rather than being cloned into it.
fn body_lines(sections: Vec<Section>, width: u16) -> Vec<Line<'static>> {
    let w = usize::from(width);
    let two = width >= TWO_COLUMN_MIN;
    let column = if two { w.saturating_sub(2) / 2 } else { w };
    // Read only where there are two columns: a one-column pane gives every field a line of its
    // own, so nothing there is measured against a budget.
    let value = column.saturating_sub(DETAIL_LABEL + 1 + GUTTER);
    let mut lines = Vec::new();
    for section in sections {
        lines.push(heading(&section.title, w));
        let mut pending: Option<Vec<Span<'static>>> = None;
        for (label, rendered) in section.fields {
            let spills = label.width() > DETAIL_LABEL || rendered.text.width() > value;
            if spills || !two {
                if let Some(spans) = pending.take() {
                    lines.push(Line::from(spans));
                }
                // Still right-aligned in the label column: only a label wider than the column
                // loses its padding, and `pair`'s `saturating_sub` is what gives it up.
                lines.push(Line::from(pair(&label, rendered)));
                continue;
            }
            match pending.take() {
                None => pending = Some(pair(&label, rendered)),
                Some(mut left) => {
                    let used: usize = left.iter().map(|s| s.content.width()).sum();
                    left.push(Span::raw(" ".repeat(column.saturating_sub(used))));
                    left.extend(pair(&label, rendered));
                    lines.push(Line::from(left));
                }
            }
        }
        if let Some(spans) = pending.take() {
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// The `ACTIONS` section: one line per action that runs on this row, drawn as a field whose
/// label is the key that runs it - so `p` sits where `Name` sits, and a reader who has never
/// pressed `a` learns the keys by reading the pane.
///
/// One per line rather than paired like the fields above: a greyed action carries the reason it
/// cannot run, and a reason cut in half is worse than one line spent.
fn action_lines(rows: &[crate::menu::Row], width: u16) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let w = usize::from(width);
    let mut lines = vec![heading("Actions", w)];
    // The same rule the `a` menu and the `⏎` picker draw, in the same place: curated workflows
    // above the names the generator lifted out of the spec.
    let raw = crate::menu::boundary(rows);
    for (i, row) in rows.iter().enumerate() {
        if raw == Some(i) {
            lines.push(Line::from(Span::styled(
                crate::ui::rule(crate::menu::RAW_CAPTION, w),
                theme::dim(),
            )));
        }
        let text = match &row.reason {
            None => row.action.title().to_string(),
            // The separator the status line and the breadcrumb already use, so a refused
            // action reads as one phrase rather than as two columns that failed to line up.
            Some(reason) => format!("{} · {reason}", row.action.title()),
        };
        lines.push(Line::from(pair(
            row.action.key,
            Rendered {
                text,
                dim: row.reason.is_some(),
            },
        )));
    }
    lines
}

/// One field: the label right-aligned and dim in [`DETAIL_LABEL`] cells, one space, then the
/// value in its own tint. A label wider than the column keeps its own width - the padding is
/// what it gives up, never a character of the label.
fn pair(label: &str, rendered: Rendered) -> Vec<Span<'static>> {
    let mut head = " ".repeat(DETAIL_LABEL.saturating_sub(label.width()));
    head.push_str(label);
    head.push(' ');
    vec![
        Span::styled(head, theme::dim()),
        Span::styled(
            rendered.text,
            if rendered.dim {
                theme::dim()
            } else {
                Style::default().fg(theme::text())
            },
        ),
    ]
}

/// `IDENTITY ─────…`: the title in the table-header style, then a dim rule to the right edge,
/// with no blank line above it.
///
/// Uppercased here rather than in `curated.toml`, so a section title reads as a phrase where it
/// is written (`UEFI boot`, `Disks (4)`) and as a heading where it is drawn.
fn heading(title: &str, width: usize) -> Line<'static> {
    let text = title.to_uppercase();
    let rule = width.saturating_sub(text.width() + 1);
    Line::from(vec![
        Span::styled(text, theme::header_row()),
        Span::styled(format!(" {}", "─".repeat(rule)), theme::dim()),
    ])
}

#[cfg(test)]
mod tests {
    // `Rendered`, `Section`, `Line`, `Span` and `body_lines` all come through here: the module's
    // own imports are the test's, and importing them again would be a duplicate definition.
    use super::*;

    /// The lines as the terminal would draw them, styles dropped.
    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn section(title: &str, fields: &[(&str, &str)]) -> Section {
        Section {
            title: title.to_string(),
            fields: fields
                .iter()
                .map(|(l, v)| {
                    (
                        (*l).to_string(),
                        Rendered {
                            text: (*v).to_string(),
                            dim: false,
                        },
                    )
                })
                .collect(),
        }
    }

    fn identity() -> Section {
        section(
            "Identity",
            &[
                ("Name", "LAB-CLEAR03"),
                ("Description", "web tier, staging"),
                ("UUID", "c429f7a2-16fe-4c5c-8eb7-bc5b9da0a746"),
                ("Project", "-"),
            ],
        )
    }

    /// At 120 cells with the sidebar open the pane's interior is 94, which gives two columns of
    /// 46 and a value column of 25. A section heading is the title uppercased and a rule to the
    /// right edge, with no blank line above it: rows are the scarce resource.
    #[test]
    fn a_wide_pane_pairs_its_fields_under_a_ruled_heading() {
        let lines = text(&body_lines(vec![identity()], 94));
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].starts_with("IDENTITY ───"), "{:?}", lines[0]);
        assert_eq!(
            lines[0].chars().count(),
            94,
            "the rule reaches the right edge"
        );
        // Labels are right-aligned in the label column, so label and value sit adjacent and the
        // eye runs down the boundary rather than down ragged whitespace.
        assert_eq!(
            lines[1].find("Name LAB-CLEAR03"),
            Some(14),
            "{:?}",
            lines[1]
        );
        assert_eq!(
            lines[1].find("Description web tier, staging"),
            Some(53),
            "the second column starts at 46 and right-aligns its label in 18: {:?}",
            lines[1]
        );
        // 36 cells against a value column of 25: the pair being built is flushed and the field
        // takes a whole line, so the identifier stays whole.
        assert_eq!(
            lines[2].find("UUID c429f7a2-16fe-4c5c-8eb7-bc5b9da0a746"),
            Some(14)
        );
        assert_eq!(lines[3].trim(), "Project -");
    }

    /// The gutter is spent out of the left column's value budget, so a paired field always
    /// leaves blank cells before the next label. The widest legal label is `DETAIL_LABEL`, whose
    /// right-aligned padding is nothing, so without the gutter a left value that filled its
    /// column would be drawn glued to that label and neither could be read. Every value width up
    /// to past the budget: each one either pairs with a gutter or takes a line of its own.
    #[test]
    fn a_paired_field_always_leaves_a_gutter_before_the_next_label() {
        let mut paired = 0;
        for len in 1..=30 {
            let value = "x".repeat(len);
            let lines = text(&body_lines(
                vec![section(
                    "State",
                    &[("Power", &value), ("Live migratable ok", "yes")],
                )],
                94,
            ));
            if lines.len() == 3 {
                continue; // The value spilled: it has the line to itself.
            }
            paired += 1;
            let at = lines[1]
                .find("Live migratable ok")
                .unwrap_or_else(|| panic!("{len}: {:?}", lines[1]));
            assert!(
                lines[1][..at].ends_with("  "),
                "{len} cells: at least two before the next label: {:?}",
                lines[1]
            );
            assert!(
                lines[1].contains(&value),
                "{len} cells: the value is not cut: {:?}",
                lines[1]
            );
        }
        assert_eq!(paired, 25, "the budget at 94 cells is 25, gutter deducted");
    }

    /// One column below 92 cells, and a label too long for the label column takes a whole line
    /// rather than being truncated into a collision with its neighbour.
    #[test]
    fn a_narrow_pane_is_one_column_and_a_long_label_spills() {
        let lines = text(&body_lines(vec![identity()], 80));
        assert_eq!(
            lines.len(),
            5,
            "a heading and one line per field: {lines:?}"
        );
        assert_eq!(lines[1].find("Name LAB-CLEAR03"), Some(14));
        assert_eq!(lines[2].find("Description web tier, staging"), Some(7));

        let long = section(
            "Controller VM",
            &[
                ("Backplane address · ipv4.value", "192.168.5.2"),
                ("Value", "192.0.2.31"),
            ],
        );
        let lines = text(&body_lines(vec![long], 94));
        assert_eq!(
            lines[1], "Backplane address · ipv4.value 192.168.5.2",
            "a thirty-cell label is not padded and is not cut"
        );
        assert_eq!(lines[2].find("Value 192.0.2.31"), Some(13));
    }

    /// A pane with no key behind it - administration's payload pane - has no kind, so there is
    /// nothing to compose sections from and nothing to curate them against: it opens raw.
    #[test]
    fn a_payload_pane_opens_raw_and_never_returns_to_a_composed_view() {
        assert_eq!(Body::default_for(None), Body::Yaml);
        assert_eq!(Body::Yaml.toggled(Body::Yaml, Body::Yaml), Body::Yaml);
        assert_eq!(Body::Yaml.toggled(Body::Json, Body::Yaml), Body::Json);
        assert_eq!(Body::Json.toggled(Body::Json, Body::Yaml), Body::Yaml);
        // An entity pane opens composed and both raw keys return to it.
        assert_eq!(
            Body::Composed.toggled(Body::Yaml, Body::Composed),
            Body::Yaml
        );
        assert_eq!(
            Body::Yaml.toggled(Body::Yaml, Body::Composed),
            Body::Composed
        );
        assert_eq!(
            Body::Json.toggled(Body::Json, Body::Composed),
            Body::Composed
        );
    }

    /// A composed body's line count depends on the pane's width, so widening the terminal - one
    /// column becomes two, and the body roughly halves - can leave a `scroll` past the new end.
    /// The first key clamps it, so `k` moves the view rather than walking a dead counter back
    /// down through lines that are no longer there.
    #[test]
    fn a_resize_that_shortens_the_body_is_repaired_by_the_next_key() {
        let mut pane = Detail::payload("t".to_string(), Value::Null);
        assert_eq!(pane.key(Key::Char('G'), 40), Action::Scrolled);
        assert_eq!(pane.scroll, 40);
        // The same pane, now twice as wide: half the lines, and the counter is past the end.
        assert_eq!(pane.key(Key::Char('k'), 20), Action::Scrolled);
        assert_eq!(pane.scroll, 19, "one line up from the new last, not 39");
    }
}
