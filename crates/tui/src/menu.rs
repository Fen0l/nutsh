//! `a`: which action to run on the row or the marks. Rows come from the row kind's
//! `action_target`, minus `hidden` ones, ordered by `order`, then `danger` ascending, then
//! label - safe first, destructive last, so a mistyped `enter` lands on something harmless.

use nutsh_catalog::Action;

use crate::key::Key;
use crate::palette::window;

/// An action, with the reason it cannot run when there is one. Greyed rows stay listed: can-i
/// can be `Unknown` or stale, and "the action is gone" is a worse bug report than "the action
/// says why".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub action: &'static Action,
    pub reason: Option<String>,
}

/// What a key did, for the app to act on. `Event`, not `Action`: `nutsh_catalog::Action` is
/// the thing being chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Filtered,
    Moved,
    Chose(&'static Action),
    Cancelled,
    Ignored,
}

pub struct Menu {
    /// What the action would run on: one row's name, or `7 Virtual Machines`.
    pub subject: String,
    rows: Vec<Row>,
    pub input: String,
    pub selected: usize,
    /// The rows the input matches; what the box shows and `selected` indexes.
    pub entries: Vec<Row>,
}

/// The caption every action surface draws where the curated workflows give way to the names the
/// generator lifted straight out of the spec.
///
/// The audit's words: the menu "mixes readable commands such as `Migrate to host` with generated
/// names such as `add-custom-attributes`". They were already ordered - the tail sinks - but a
/// list with no rule through it reads as one list, and the reader has no way to know that the
/// half below is untested, unlabelled and unranked by anybody.
pub const RAW_CAPTION: &str = " raw API names ";

/// Where those names begin in `rows`: the index of the first uncurated one, when the list holds
/// some of each. `None` when it is all one or all the other, because a box with nothing to
/// separate draws no rule.
///
/// `order == 0` is the test, and it is the same one [`sort`] sinks on: the overlay gives every
/// action it curates an `order`, and one it has never seen keeps the default.
pub fn boundary(rows: &[Row]) -> Option<usize> {
    let first = rows.iter().position(|r| r.action.order == 0)?;
    (first > 0).then_some(first)
}

/// The order every action surface lists in: `order` when the overlay curated one, then `danger`
/// ascending, then the label - safe first, destructive last, so a mistyped `enter` lands on
/// something harmless. An uncurated action carries `order: 0` and `danger: None`, which would
/// otherwise put the whole uncurated tail above the headline set, so it sinks instead.
///
/// A free function rather than [`Menu::open`]'s business alone: the `⏎` picker's actions group
/// and the detail pane's actions section list the same rows, and three orders would be three
/// lists.
pub fn sort(rows: &mut [Row]) {
    let key = |r: &Row| {
        (
            r.action.order == 0,
            r.action.order,
            r.action.danger,
            r.action.title(),
        )
    };
    rows.sort_by(|a, b| key(a).cmp(&key(b)));
}

impl Menu {
    pub fn open(subject: String, mut rows: Vec<Row>) -> Menu {
        sort(&mut rows);
        let mut menu = Menu {
            subject,
            rows,
            input: String::new(),
            selected: 0,
            entries: Vec::new(),
        };
        menu.refresh();
        menu
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
                self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
                Event::Moved
            }
            Key::Up | Key::BackTab => {
                self.selected = self.selected.saturating_sub(1);
                Event::Moved
            }
            Key::Enter => match self.current() {
                Some(row) => Event::Chose(row.action),
                None => Event::Ignored,
            },
            _ => Event::Ignored,
        }
    }

    /// Typing narrows rather than filters away: an empty input shows every action.
    fn refresh(&mut self) {
        let needle = self.input.to_ascii_lowercase();
        self.entries = self
            .rows
            .iter()
            .filter(|r| {
                needle.is_empty()
                    || r.action.title().to_ascii_lowercase().contains(&needle)
                    || r.action.name.contains(&needle)
            })
            .cloned()
            .collect();
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }

    pub fn current(&self) -> Option<&Row> {
        self.entries.get(self.selected)
    }

    /// The `rows` entries to draw and the index the first of them has, framing the selection.
    pub fn window(&self, rows: usize) -> (usize, &[Row]) {
        window(&self.entries, self.selected, rows)
    }

    /// What the box draws, and where the cursor is in it: the entries with [`RAW_CAPTION`]
    /// inserted where the curated workflows give way to the generated names.
    ///
    /// The rule is a row of this list rather than a decoration on the box, for the reason the
    /// picker's captions are: the filter moves it, the scroll window can start below it, and a
    /// caption drawn outside the list would go on naming rows that are no longer under it.
    /// `None` for the cursor when the filter matched nothing - there is then nothing to select
    /// and the box says so.
    pub fn shown(&self) -> (Vec<Shown<'_>>, Option<usize>) {
        let at = boundary(&self.entries);
        let mut rows = Vec::with_capacity(self.entries.len() + usize::from(at.is_some()));
        let mut cursor = None;
        for (i, row) in self.entries.iter().enumerate() {
            if at == Some(i) {
                rows.push(Shown::Boundary);
            }
            if i == self.selected {
                cursor = Some(rows.len());
            }
            rows.push(Shown::Row(row));
        }
        // `None` falls out of the loop rather than being decided: nothing is selected exactly
        // when no entry carries the cursor, which is what an empty filter result looks like.
        (rows, cursor)
    }
}

/// A row of the drawn box: an action, or the rule under the last curated workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown<'a> {
    Row(&'a Row),
    Boundary,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind() -> &'static nutsh_catalog::Kind {
        nutsh_catalog::kind("vmm.ahv.config.Vm").expect("the catalog has VMs")
    }

    fn menu() -> Menu {
        let rows = kind()
            .actions
            .iter()
            .filter(|a| !a.hidden)
            .map(|action| Row {
                action,
                reason: None,
            })
            .collect();
        Menu::open("web-01".into(), rows)
    }

    /// The curated headline set comes first in its curated order, and the uncurated tail sinks
    /// below it whatever its name sorts as.
    #[test]
    fn curated_actions_lead_in_order_and_the_rest_sink() {
        let menu = menu();
        let names: Vec<&str> = menu.entries.iter().map(|r| r.action.name).collect();
        assert_eq!(
            &names[..4],
            &["power-on", "power-off", "power-cycle", "reset"],
            "{names:?}"
        );
        let curated = names
            .iter()
            .position(|n| *n == "delete")
            .expect("delete is curated");
        assert!(
            names[curated + 1..].iter().all(|n| menu
                .entries
                .iter()
                .any(|r| r.action.name == *n && r.action.order == 0)),
            "{names:?}"
        );
    }

    /// Typing narrows on the label as well as the catalog name, and `enter` chooses what is
    /// under the cursor.
    #[test]
    fn typing_narrows_and_enter_chooses() {
        let mut menu = menu();
        let all = menu.entries.len();
        for c in "recovery".chars() {
            assert_eq!(menu.key(Key::Char(c)), Event::Filtered);
        }
        assert!(menu.entries.len() < all);
        assert_eq!(
            menu.key(Key::Enter),
            Event::Chose(kind().action("snapshot").expect("VMs snapshot"))
        );
    }

    /// A filter nothing matches leaves nothing selected, and `enter` on it does nothing rather
    /// than indexing off the end.
    #[test]
    fn an_empty_filter_result_ignores_enter() {
        let mut menu = menu();
        for c in "zzz".chars() {
            menu.key(Key::Char(c));
        }
        assert!(menu.entries.is_empty());
        assert_eq!(menu.key(Key::Enter), Event::Ignored);
        assert_eq!(menu.key(Key::Down), Event::Moved);
        assert_eq!(menu.selected, 0);
    }
}
