//! The generic form: an action's `Field`s as a labelled column. It collects strings only;
//! `core::actions::build_body` validates, substitutes, and types them.
//!
//! The reference picker lives here too, because it is the form's: it opens over one field, it
//! writes one field, and it closes back onto the form. `crate::picker::Picker` is a different
//! thing with the same shape - the child *kinds* of a row, not the *rows* of a kind.

use std::collections::HashMap;

use nutsh_catalog::{Field, FieldType, Kind};
use nutsh_core::store::Failure;

use crate::key::Key;
use crate::palette::window;
use crate::text::{Edit, Input};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Moved,
    Edited,
    Submit,
    Cancelled,
    /// `enter` on a `Reference`: the app opens a picker over the referenced kind.
    Pick(&'static str),
    Ignored,
}

pub struct Form {
    pub title: String,
    /// Visible fields only; `hidden` ones never appear and are filled from their `value`.
    pub fields: Vec<&'static Field>,
    values: Vec<Input>,
    pub selected: usize,
    pub error: Option<String>,
    /// `loading…` while a reference picker's list subscription is in flight.
    pub loading: bool,
    /// Why the reference picker's list came back with nothing, when it failed rather than
    /// being empty. `loading` alone cannot tell the two apart, and "no rows" for an
    /// unreachable kind is a lie the user acts on.
    pub listing_error: Option<Failure>,
    /// The kinds this form has already asked for a one-shot list of. `enter`, `esc`, `enter`
    /// on the same reference would otherwise start a second walk over the same pages while
    /// the first is still running.
    listed: Vec<&'static str>,
    /// The reference picker over the selected field's kind, while it is open.
    pub picker: Option<RefPicker>,
}

impl Form {
    /// A blank form: every field starts at its seed. What a `POST` that makes something new
    /// opens with, where a skeleton is the shape of the thing to fill in.
    pub fn open(title: &str, fields: &'static [Field]) -> Form {
        Form::over(title, fields, None)
    }

    /// The same form, over an entity that already exists: `current` is what it has, from
    /// `nutsh_core::actions::current_values`.
    ///
    /// A current value wins over the field's own seed, because that is the whole point: an
    /// update edits what is there. A field the entity has **no** value for is where the two
    /// cases part. A blank form offers the shape - a JSON skeleton, an enum's first option -
    /// because there is nothing it could overwrite. An edit offers nothing, because
    /// `build_body` sends whatever a field holds and `Client::update` merges that over a fresh
    /// `GET`: a field left empty keeps what the Prism Central has, and a field holding a
    /// skeleton nobody typed replaces it. Sixty of the seventy-one `PUT` and `PATCH` forms are
    /// over kinds the scheduler narrows, so most of their fields are outside the `$select` and
    /// have no current value to offer.
    ///
    /// A curated `value` is the exception, and stands in both: it is this program's own
    /// instruction about what that field must carry, not a guess at what the entity has.
    pub fn over(
        title: &str,
        fields: &'static [Field],
        current: Option<&HashMap<&str, String>>,
    ) -> Form {
        let editing = current.is_some();
        let current = current.cloned().unwrap_or_default();
        let visible: Vec<&'static Field> = fields.iter().filter(|f| !f.hidden).collect();
        let values = visible
            .iter()
            .map(|f| match (current.get(f.name), &f.ty, f.value) {
                (Some(now), _, _) => Input::from(now.as_str()),
                (_, _, Some(v)) if !v.starts_with('$') => Input::from(v),
                // Nothing invented over an existing entity: see above.
                _ if editing => Input::default(),
                // A seed the user edits; a substitution is expanded at submit, not shown raw.
                (_, FieldType::Json(seed), None) => Input::from(*seed),
                (_, FieldType::Enum(vs), _) => {
                    Input::from(offered(vs).first().copied().unwrap_or(""))
                }
                _ => Input::default(),
            })
            .collect();
        Form {
            title: title.to_string(),
            fields: visible,
            values,
            selected: 0,
            error: None,
            loading: false,
            listing_error: None,
            listed: Vec::new(),
            picker: None,
        }
    }

    /// Whether a one-shot list for `kind` has already been started from this form. Recorded on
    /// the first ask, so a second `enter` on the same reference reopens the box over whatever
    /// the store has rather than walking the pages again.
    pub(crate) fn first_listing_of(&mut self, kind: &'static str) -> bool {
        if self.listed.contains(&kind) {
            return false;
        }
        self.listed.push(kind);
        true
    }

    pub fn key(&mut self, key: Key) -> Event {
        let Some(field) = self.fields.get(self.selected).copied() else {
            // Every field hidden: there is nothing to edit and nowhere to move, but the form
            // is still what fills the body from those fields' `value`s, so the two keys that
            // do not name a field have to go on working.
            return match key {
                Key::Enter => Event::Submit,
                Key::Esc => Event::Cancelled,
                _ => Event::Ignored,
            };
        };
        // The picker's two keys, taken before the rest because both depend on whether the
        // field is already filled. `enter` lists only while it is empty - a reference that has
        // been chosen has to be submittable, or a form whose one field is a reference could
        // never be sent at all - and `space` is what reopens the list to change it.
        if let FieldType::Reference(Some(kind)) = field.ty {
            match key {
                Key::Enter if self.values[self.selected].is_empty() => return Event::Pick(kind),
                Key::Char(' ') => return Event::Pick(kind),
                _ => {}
            }
        }
        match key {
            Key::Esc => Event::Cancelled,
            Key::Enter => Event::Submit,
            Key::Tab | Key::Down => {
                self.selected = (self.selected + 1).min(self.fields.len().saturating_sub(1));
                Event::Moved
            }
            Key::BackTab | Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                Event::Moved
            }
            // `←`/`→` cycle an enum. Every other field type falls past this to the cursor.
            Key::Left | Key::Right if matches!(field.ty, FieldType::Enum(_)) => {
                let FieldType::Enum(all) = field.ty else {
                    return Event::Ignored;
                };
                let values = offered(all);
                if values.is_empty() {
                    return Event::Ignored;
                }
                let current = values
                    .iter()
                    .position(|v| *v == self.values[self.selected].as_str())
                    .unwrap_or(0);
                let next = if key == Key::Right {
                    (current + 1) % values.len()
                } else {
                    (current + values.len() - 1) % values.len()
                };
                self.values[self.selected] = Input::from(values[next]);
                self.error = None;
                Event::Edited
            }
            Key::Char(' ') if field.ty == FieldType::Bool => {
                let on = self.values[self.selected].as_str() == "true";
                self.values[self.selected] = Input::from((!on).to_string());
                self.error = None;
                Event::Edited
            }
            // A checkbox and an enum are not typed into, moved through, or deleted from. Both are
            // drawn from their value as a whole word and `build_body` reads them the same way, so
            // one stray letter is a box that silently unticks (`"true"` plus an `x` is not
            // `"true"`) or an enum variant Prism Central has never heard of. One arm now rather
            // than two, because there are eight editing keys to refuse rather than two.
            _ if matches!(field.ty, FieldType::Bool | FieldType::Enum(_)) => Event::Ignored,
            // An integer refuses a letter on input rather than at submit, where the message
            // would arrive after everything else was typed.
            Key::Char(c) if field.ty == FieldType::Integer && !c.is_ascii_digit() => Event::Ignored,
            key => match self.values[self.selected].key(key) {
                Some(Edit::Changed) => {
                    self.error = None;
                    Event::Edited
                }
                Some(Edit::Moved) => Event::Moved,
                Some(Edit::Ignored) | None => Event::Ignored,
            },
        }
    }

    pub fn value(&self, name: &str) -> &str {
        self.fields
            .iter()
            .position(|f| f.name == name)
            .map_or("", |i| self.values[i].as_str())
    }

    /// Where the block goes on the selected field. An `Enum` and a `Bool` are drawn as whole
    /// words and take no cursor, so `ui` asks only for the fields that are typed into.
    pub fn cursor(&self) -> usize {
        self.values.get(self.selected).map_or(0, Input::cursor)
    }

    /// What `build_body` collects. Only visible fields are here; the hidden ones are filled
    /// from their `value`, which is the point of `hidden`.
    pub fn entered(&self) -> HashMap<&'static str, String> {
        self.fields
            .iter()
            .zip(&self.values)
            .map(|(f, v)| (f.name, v.as_str().to_string()))
            .collect()
    }

    /// The chosen row of a reference picker.
    pub fn set_selected_value(&mut self, value: String) {
        if let Some(slot) = self.values.get_mut(self.selected) {
            *slot = Input::from(value);
        }
        self.loading = false;
    }
}

/// The variants a form offers. `$UNKNOWN` and `$REDACTED` are what a v4 enum answers *with* -
/// the generator carries them because the schema does - and never something to send;
/// `build_body` passes an entered string through verbatim, so they are dropped here rather
/// than left for the user to cycle onto. An enum of nothing else keeps them: an empty cycle
/// would be worse than a wrong one.
fn offered(values: &'static [&'static str]) -> Vec<&'static str> {
    let kept: Vec<&'static str> = values
        .iter()
        .copied()
        .filter(|v| !v.starts_with('$'))
        .collect();
    if kept.is_empty() {
        values.to_vec()
    } else {
        kept
    }
}

/// One row of the kind a `Reference` field names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub ext_id: String,
    pub name: String,
}

/// What a key did in the reference picker, for the app to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Filtered,
    Moved,
    /// The extId of the chosen row: what the field is written with.
    Chose(String),
    Cancelled,
    Ignored,
}

/// The rows of the kind a `Reference` field names. It opens on whatever the store already
/// holds and is refilled as a list lands, so a kind nothing has listed yet shows an empty box
/// that fills in rather than a form that appears to have swallowed the key.
pub struct RefPicker {
    /// The kind being listed: also the table the app refills the rows from.
    pub kind: &'static Kind,
    /// The field it was opened over, for the box's title.
    pub label: &'static str,
    rows: Vec<Choice>,
    pub input: String,
    pub selected: usize,
    /// The rows the input matches; what the box shows and `selected` indexes.
    pub entries: Vec<Choice>,
}

impl RefPicker {
    pub fn open(kind: &'static Kind, label: &'static str, rows: Vec<Choice>) -> RefPicker {
        let mut picker = RefPicker {
            kind,
            label,
            rows,
            input: String::new(),
            selected: 0,
            entries: Vec::new(),
        };
        picker.refresh();
        picker
    }

    /// The rows a later list cycle brought, keeping the filter and framing the selection.
    pub fn set_rows(&mut self, rows: Vec<Choice>) {
        self.rows = rows;
        self.refresh();
    }

    pub fn key(&mut self, key: Key) -> Picked {
        match key {
            Key::Esc => Picked::Cancelled,
            Key::Char(c) => {
                self.input.push(c);
                self.selected = 0;
                self.refresh();
                Picked::Filtered
            }
            Key::Backspace => {
                self.input.pop();
                self.selected = 0;
                self.refresh();
                Picked::Filtered
            }
            Key::Down | Key::Tab => {
                self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
                Picked::Moved
            }
            Key::Up | Key::BackTab => {
                self.selected = self.selected.saturating_sub(1);
                Picked::Moved
            }
            Key::Enter => match self.entries.get(self.selected) {
                Some(choice) => Picked::Chose(choice.ext_id.clone()),
                None => Picked::Ignored,
            },
            _ => Picked::Ignored,
        }
    }

    /// Typing narrows rather than filters away: an empty input shows every row.
    fn refresh(&mut self) {
        let needle = self.input.to_ascii_lowercase();
        self.entries = self
            .rows
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.name.to_ascii_lowercase().contains(&needle)
                    || c.ext_id.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }

    /// The `rows` entries to draw and the index the first of them has, framing the selection.
    pub fn window(&self, rows: usize) -> (usize, &[Choice]) {
        window(&self.entries, self.selected, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind() -> &'static Kind {
        nutsh_catalog::kind("clustermgmt.config.Cluster").expect("the catalog has clusters")
    }

    fn choices() -> Vec<Choice> {
        ["alpha", "beta", "gamma"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| Choice {
                ext_id: format!("id-{i}"),
                name: name.to_string(),
            })
            .collect()
    }

    /// A `Reference` field asks for a picker instead of submitting, so `enter` on it can never
    /// send a body with the field still empty.
    #[test]
    fn enter_on_a_reference_asks_for_a_picker() {
        static FIELDS: &[Field] = &[
            Field {
                name: "cluster",
                label: "Cluster",
                ty: FieldType::Reference(Some("clustermgmt.config.Cluster")),
                required: true,
                value: None,
                hidden: false,
            },
            Field {
                name: "name",
                label: "Name",
                ty: FieldType::Text,
                required: false,
                value: None,
                hidden: false,
            },
        ];
        let mut form = Form::open("Migrate", FIELDS);
        assert_eq!(
            form.key(Key::Enter),
            Event::Pick("clustermgmt.config.Cluster")
        );
        assert_eq!(form.key(Key::Tab), Event::Moved);
        assert_eq!(
            form.key(Key::Enter),
            Event::Submit,
            "an ordinary field sends"
        );
    }

    /// The picker writes the chosen row's extId into the field it was opened over, not the
    /// name the row is listed by: the body carries the reference.
    #[test]
    fn the_picker_narrows_and_writes_the_ext_id() {
        static FIELDS: &[Field] = &[Field {
            name: "cluster",
            label: "Cluster",
            ty: FieldType::Reference(Some("clustermgmt.config.Cluster")),
            required: true,
            value: None,
            hidden: false,
        }];
        let mut form = Form::open("Migrate", FIELDS);
        let mut picker = RefPicker::open(kind(), "Cluster", choices());
        assert_eq!(picker.entries.len(), 3);
        assert_eq!(picker.key(Key::Char('m')), Picked::Filtered);
        assert_eq!(picker.entries.len(), 1, "gamma alone");
        assert_eq!(picker.key(Key::Enter), Picked::Chose("id-2".into()));
        form.set_selected_value("id-2".into());
        assert_eq!(form.value("cluster"), "id-2");
        assert!(!form.loading);
    }

    /// A filter nothing matches leaves nothing selected, and `enter` on it does nothing rather
    /// than indexing off the end - the same contract the menu and the kind picker keep.
    #[test]
    fn an_empty_result_ignores_enter() {
        let mut picker = RefPicker::open(kind(), "Cluster", choices());
        for c in "zzz".chars() {
            picker.key(Key::Char(c));
        }
        assert!(picker.entries.is_empty());
        assert_eq!(picker.key(Key::Enter), Picked::Ignored);
        assert_eq!(picker.key(Key::Down), Picked::Moved);
        assert_eq!(picker.selected, 0);
    }

    /// A picker opened before the list landed refills from the cycle that follows, keeping the
    /// filter that was typed while it was empty.
    #[test]
    fn rows_that_land_later_refill_the_picker() {
        let mut picker = RefPicker::open(kind(), "Cluster", Vec::new());
        assert_eq!(picker.key(Key::Char('a')), Picked::Filtered);
        assert!(picker.entries.is_empty());
        picker.set_rows(choices());
        assert_eq!(
            picker
                .entries
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"],
            "every name holds an 'a'"
        );
    }

    /// A one-shot list is asked for once per kind per form: `enter`, `esc`, `enter` on the
    /// same reference must not put a second page walk in flight beside the first.
    #[test]
    fn a_kind_is_listed_once_per_form() {
        static FIELDS: &[Field] = &[Field {
            name: "cluster",
            label: "Cluster",
            ty: FieldType::Reference(Some("clustermgmt.config.Cluster")),
            required: true,
            value: None,
            hidden: false,
        }];
        let mut form = Form::open("Migrate", FIELDS);
        assert!(form.first_listing_of("clustermgmt.config.Cluster"));
        assert!(!form.first_listing_of("clustermgmt.config.Cluster"));
        assert!(
            form.first_listing_of("clustermgmt.config.Host"),
            "another kind is its own"
        );
    }

    /// A form whose fields are every one of them hidden has nothing to edit, but it is still
    /// what fills the body: `enter` submits it rather than indexing an empty column.
    #[test]
    fn a_form_with_only_hidden_fields_still_submits() {
        static FIELDS: &[Field] = &[Field {
            name: "vmExtId",
            label: "",
            ty: FieldType::Text,
            required: true,
            value: Some("$ext_id"),
            hidden: true,
        }];
        let mut form = Form::open("Snapshot", FIELDS);
        assert!(form.fields.is_empty());
        assert_eq!(form.key(Key::Char('x')), Event::Ignored);
        assert_eq!(form.key(Key::Enter), Event::Submit);
        assert_eq!(form.key(Key::Esc), Event::Cancelled);
        assert_eq!(form.entered().len(), 0, "a hidden field is not collected");
    }
}
