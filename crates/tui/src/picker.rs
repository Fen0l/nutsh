//! `enter` on a row: what can be opened **under** it, and what can be **done to** it. It owns
//! its filter and its scroll window, and it knows which of either the row cannot reach.
//!
//! The two groups are captioned, and a caption is a row of the list rather than a decoration on
//! the box: the filter can empty a group, the window can start halfway down one, and a caption
//! that lived outside the list would go on naming rows that are no longer under it.

use nutsh_catalog::{Action, Kind, Reach, reach};

use crate::key::Key;
use crate::menu;
use crate::palette::window;

/// The groups the box lists, in the order it lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// The child kinds this row can be drilled into.
    Related,
    /// The actions that run on this row, as somebody curated them.
    Actions,
    /// The rest of the actions, under the names the generator lifted out of the spec.
    Raw,
}

impl Group {
    /// What the caption reads. Lower case with its own spaces, as the menu's `by namespace`
    /// caption is: it is a rule through the list, not a heading over a page.
    pub fn caption(self) -> &'static str {
        match self {
            Group::Related => " related ",
            Group::Actions => " actions ",
            Group::Raw => crate::menu::RAW_CAPTION,
        }
    }

    /// How deeply a caption is nested: a group's own rule, or a rule *inside* a group.
    ///
    /// `refresh` needs it to decide which pending captions a matching row keeps. `related` and
    /// `actions` are siblings, so a row under `actions` must not drag `related` back onto the
    /// frame; `raw API names` sits inside `actions`, so a row under it keeps both.
    fn level(self) -> u8 {
        match self {
            Group::Related | Group::Actions => 0,
            Group::Raw => 1,
        }
    }
}

/// A row of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// The rule that names the group below it. Never selectable, and never last: `refresh`
    /// writes one only when something matched under it.
    Caption(Group),
    /// A child kind, with the reason it cannot be opened under this row when there is one.
    Child {
        kind: &'static Kind,
        reason: Option<String>,
    },
    /// An action on this row, with the reason it cannot run when there is one.
    Act {
        action: &'static Action,
        reason: Option<String>,
    },
}

impl Entry {
    /// Why this row cannot be taken, when it cannot.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Entry::Caption(_) => None,
            Entry::Child { reason, .. } | Entry::Act { reason, .. } => reason.as_deref(),
        }
    }

    fn is_caption(&self) -> bool {
        matches!(self, Entry::Caption(_))
    }

    /// Whether typing `needle` keeps this row. A caption never answers: `refresh` decides its
    /// fate by what matched under it.
    fn matches(&self, needle: &str) -> bool {
        match self {
            Entry::Caption(_) => false,
            Entry::Child { kind, .. } => {
                kind.display.to_ascii_lowercase().contains(needle)
                    || kind.id.to_ascii_lowercase().contains(needle)
            }
            Entry::Act { action, .. } => {
                action.title().to_ascii_lowercase().contains(needle) || action.name.contains(needle)
            }
        }
    }
}

/// What a key did, for the app to act on. `Event`, not `Action`: `nutsh_catalog::Action` is one
/// of the two things being chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Filtered,
    Moved,
    /// Open this child kind under the row.
    Chose(&'static Kind),
    /// Run this action on the row.
    Run(&'static Action),
    Cancelled,
    Ignored,
}

pub struct Picker {
    pub parent: &'static Kind,
    pub parent_ext_id: String,
    pub parent_name: String,
    /// Every child of the parent kind and every action on the row, captions included, in the
    /// order the box lists them.
    rows: Vec<Entry>,
    pub input: String,
    pub selected: usize,
    /// The rows the input matches; what the box shows and `selected` indexes.
    pub entries: Vec<Entry>,
}

impl Picker {
    /// Every child of `parent`, greyed by the catalog for a kind this row cannot reach - one
    /// that still needs a query parameter of its own - and by `unavailable` for one this
    /// Prism Central does not serve; then `actions`, which the caller has already greyed with
    /// the one refusal string every action surface shares.
    pub fn open(
        parent: &'static Kind,
        parent_ext_id: String,
        parent_name: String,
        unavailable: impl Fn(&'static Kind) -> Option<String>,
        actions: Vec<menu::Row>,
    ) -> Picker {
        let children: Vec<Entry> = nutsh_catalog::children(parent.id)
            .into_iter()
            .map(|kind| Entry::Child {
                kind,
                reason: match reach(kind) {
                    Reach::FromParent(_) => unavailable(kind),
                    other => other.reason(),
                },
            })
            .collect();
        let mut rows = Vec::new();
        if !children.is_empty() {
            rows.push(Entry::Caption(Group::Related));
            rows.extend(children);
        }
        if !actions.is_empty() {
            // The same rule the `a` menu draws, in the same place: curated workflows above the
            // names the generator lifted out of the spec.
            let raw = menu::boundary(&actions);
            rows.push(Entry::Caption(Group::Actions));
            for (i, row) in actions.into_iter().enumerate() {
                if raw == Some(i) {
                    rows.push(Entry::Caption(Group::Raw));
                }
                rows.push(Entry::Act {
                    action: row.action,
                    reason: row.reason,
                });
            }
        }
        let mut picker = Picker {
            parent,
            parent_ext_id,
            parent_name,
            rows,
            input: String::new(),
            selected: 0,
            entries: Vec::new(),
        };
        picker.refresh();
        picker
    }

    pub fn key(&mut self, key: Key) -> Event {
        match key {
            Key::Esc => Event::Cancelled,
            Key::Char(c) => {
                self.input.push(c);
                self.selected = 0;
                self.refresh();
                Event::Filtered
            }
            Key::Backspace => {
                self.input.pop();
                self.selected = 0;
                self.refresh();
                Event::Filtered
            }
            Key::Down | Key::Tab => {
                let next = (self.selected + 1).min(self.entries.len().saturating_sub(1));
                self.selected = self.past_caption(next, true);
                Event::Moved
            }
            Key::Up | Key::BackTab => {
                let next = self.selected.saturating_sub(1);
                self.selected = self.past_caption(next, false);
                Event::Moved
            }
            Key::Enter => match self.current() {
                Some(Entry::Child { kind, .. }) => Event::Chose(kind),
                Some(Entry::Act { action, .. }) => Event::Run(action),
                Some(Entry::Caption(_)) | None => Event::Ignored,
            },
            _ => Event::Ignored,
        }
    }

    /// Typing narrows rather than filters away: an empty input shows every row. A caption
    /// survives only when something under it did, so an emptied group leaves no orphan rule.
    fn refresh(&mut self) {
        let needle = self.input.to_ascii_lowercase();
        let mut entries = Vec::new();
        // A list, not one caption: `actions` and `raw API names` sit together when the filter
        // leaves nothing curated, and keeping only the innermost would file the survivors under
        // a rule that does not name their group.
        let mut pending: Vec<&Entry> = Vec::new();
        for row in &self.rows {
            if let Entry::Caption(group) = row {
                // Only the captions this one sits inside survive it: a new group's rule
                // replaces the last group's, and a rule within a group keeps the group's.
                pending.retain(|p| matches!(p, Entry::Caption(g) if g.level() < group.level()));
                pending.push(row);
                continue;
            }
            if !needle.is_empty() && !row.matches(&needle) {
                continue;
            }
            entries.extend(pending.drain(..).cloned());
            entries.push(row.clone());
        }
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.selected = self.past_caption(self.selected, true);
    }

    /// Put the cursor on row `index`, for the mouse - which addresses a row by where it is on
    /// the frame rather than by stepping. `false` when there is no such row, or when it is a
    /// caption: a click on a rule selects nothing.
    pub fn select(&mut self, index: usize) -> bool {
        match self.entries.get(index) {
            None | Some(Entry::Caption(_)) => false,
            Some(_) => {
                self.selected = index;
                true
            }
        }
    }

    /// `index` unless it is a caption, in which case the nearest row the way the cursor was
    /// going, or - at the end of the list - the other way. A run rather than a step: two
    /// captions sit together when the filter leaves a group with nothing curated in it.
    fn past_caption(&self, index: usize, down: bool) -> usize {
        let takes_the_cursor = |i: &usize| !self.entries.get(*i).is_some_and(Entry::is_caption);
        if takes_the_cursor(&index) {
            return index;
        }
        let last = self.entries.len().saturating_sub(1);
        let forward = || (index..=last).find(takes_the_cursor);
        let back = || (0..index).rev().find(takes_the_cursor);
        if down {
            forward().or_else(back)
        } else {
            back().or_else(forward)
        }
        .unwrap_or(index)
    }

    /// Whether the parent kind has no children at all; `enter` then means the detail pane,
    /// whose own actions section lists what this box would have listed under `actions`.
    pub fn is_empty(&self) -> bool {
        !self.rows.iter().any(|e| matches!(e, Entry::Child { .. }))
    }

    /// The `rows` entries to draw and the index the first of them has, framing the selection.
    pub fn window(&self, rows: usize) -> (usize, &[Entry]) {
        window(&self.entries, self.selected, rows)
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(id: &str) -> &'static Kind {
        nutsh_catalog::kind(id).unwrap_or_else(|| panic!("the catalog has {id}"))
    }

    /// The actions of `id`, as the app hands them over: none refused.
    fn actions(id: &str) -> Vec<menu::Row> {
        let mut rows: Vec<menu::Row> = kind(id)
            .action_target()
            .actions
            .iter()
            .filter(|a| !a.hidden && a.acts_on_a_row())
            .map(|action| menu::Row {
                action,
                reason: None,
            })
            .collect();
        menu::sort(&mut rows);
        rows
    }

    fn vm_picker() -> Picker {
        Picker::open(
            kind("vmm.ahv.config.Vm"),
            "vm-ext-id".into(),
            "web-01".into(),
            |_| None,
            actions("vmm.ahv.config.Vm"),
        )
    }

    fn children(picker: &Picker) -> Vec<&'static str> {
        picker
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Child { kind, .. } => Some(kind.id),
                _ => None,
            })
            .collect()
    }

    /// A child whose list path still holds a placeholder the parent cannot fill is greyed
    /// with the catalog's reason, whatever the session says: a file server's replication
    /// policy id is nothing this row supplies.
    #[test]
    fn a_child_that_needs_a_parameter_is_greyed() {
        let picker = Picker::open(
            kind("files.config.FileServer"),
            "fs-ext-id".into(),
            "fs-01".into(),
            |_| None,
            Vec::new(),
        );
        let entry = picker
            .entries
            .iter()
            .find(|e| matches!(e, Entry::Child { kind, .. } if kind.id == "files.config.VdiUserSession"))
            .expect("file servers have VDI user sessions as a child");
        assert_eq!(entry.reason(), Some("needs a parameter"));
        // The children that only need the file server itself stay openable.
        assert!(
            picker
                .entries
                .iter()
                .any(|e| matches!(e, Entry::Child { .. }) && e.reason().is_none()),
            "{:?}",
            children(&picker)
        );
    }

    /// The session's reason reaches the entries too, for a child of a kind this Prism Central
    /// serves at a version older than the child's.
    #[test]
    fn a_child_the_session_cannot_serve_is_greyed() {
        let picker = Picker::open(
            kind("vmm.ahv.config.Vm"),
            "vm-ext-id".into(),
            "web-01".into(),
            |k| (k.id == "vmm.ahv.config.Nic").then(|| "namespace not served".to_string()),
            Vec::new(),
        );
        let nic = picker
            .entries
            .iter()
            .find(|e| matches!(e, Entry::Child { kind, .. } if kind.id == "vmm.ahv.config.Nic"))
            .expect("VMs have NICs as a child");
        assert_eq!(nic.reason(), Some("namespace not served"));
    }

    #[test]
    fn typing_narrows_the_children() {
        let mut picker = vm_picker();
        let all = children(&picker).len();
        assert_eq!(picker.key(Key::Char('d')), Event::Filtered);
        assert_eq!(picker.key(Key::Char('i')), Event::Filtered);
        assert_eq!(picker.key(Key::Char('s')), Event::Filtered);
        assert!(children(&picker).len() < all);
        assert_eq!(
            picker.key(Key::Enter),
            Event::Chose(kind("vmm.ahv.config.Disk"))
        );
    }

    /// The two groups, each under its own caption, and the cursor never stands on a rule:
    /// it opens below the first one and steps over the second.
    #[test]
    fn the_actions_group_follows_the_related_one_and_the_cursor_skips_both_captions() {
        let picker = vm_picker();
        assert_eq!(
            picker.entries.first(),
            Some(&Entry::Caption(Group::Related))
        );
        let captions: Vec<&Entry> = picker.entries.iter().filter(|e| e.is_caption()).collect();
        assert_eq!(
            captions,
            vec![
                &Entry::Caption(Group::Related),
                &Entry::Caption(Group::Actions),
                &Entry::Caption(Group::Raw),
            ],
            "the two groups, and the rule inside the second"
        );
        assert_eq!(picker.selected, 1, "never on the caption");
        // Walking down from the top steps over the second caption without ever landing on it.
        let mut picker = picker;
        for _ in 0..picker.entries.len() {
            picker.key(Key::Down);
            assert!(
                !picker.entries[picker.selected].is_caption(),
                "stopped on a rule at {}",
                picker.selected
            );
        }
        // And back up again.
        for _ in 0..picker.entries.len() {
            picker.key(Key::Up);
            assert!(!picker.entries[picker.selected].is_caption());
        }
        assert_eq!(picker.selected, 1);
    }

    /// `enter` on an action asks for the action rather than for a table, and a filter that
    /// empties the related group takes its caption with it.
    #[test]
    fn enter_on_an_action_runs_it_and_an_empty_group_loses_its_caption() {
        let mut picker = vm_picker();
        for c in "power on".chars() {
            picker.key(Key::Char(c));
        }
        assert!(
            picker
                .entries
                .iter()
                .all(|e| !matches!(e, Entry::Child { .. })),
            "no child is called power on"
        );
        assert_eq!(
            picker.entries.first(),
            Some(&Entry::Caption(Group::Actions)),
            "the related caption went with its rows"
        );
        assert!(
            !picker
                .entries
                .iter()
                .any(|e| matches!(e, Entry::Caption(Group::Raw))),
            "and no rule is drawn where nothing generated matched"
        );
        let power_on = kind("vmm.ahv.config.Vm")
            .action("power-on")
            .expect("VMs power on");
        assert_eq!(picker.key(Key::Enter), Event::Run(power_on));
    }

    /// A click on a rule selects nothing; a click on a row selects it.
    #[test]
    fn a_click_on_a_caption_selects_nothing() {
        let mut picker = vm_picker();
        assert!(!picker.select(0), "row 0 is the related caption");
        assert!(picker.select(2));
        assert_eq!(picker.selected, 2);
    }

    /// A kind with no children opens the detail pane instead, whatever its action list holds.
    #[test]
    fn a_kind_with_no_children_is_empty_however_many_actions_it_has() {
        let id = "monitoring.serviceability.Alert";
        let picker = Picker::open(
            kind(id),
            "alert-ext-id".into(),
            "an alert".into(),
            |_| None,
            actions(id),
        );
        assert!(nutsh_catalog::children(id).is_empty());
        assert!(
            !actions(id).is_empty(),
            "alerts are acknowledged and resolved"
        );
        assert!(picker.is_empty());
    }
}
