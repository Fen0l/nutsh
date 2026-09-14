//! `:search <term>`: what every kind already loaded holds under that term, grouped by kind,
//! with the reach stated underneath.
//!
//! The reach line is not a footnote. This searches the store - what the session has polled plus
//! whatever the on-disk cache painted in before the first frame - and never the API, so a term
//! that finds nothing may mean "there is no such VM" or may mean "the VM table was never
//! opened". A list that did not say which of the two it was would be a confident wrong answer,
//! so the count of kinds looked at and the count of kinds not loaded travel with every result,
//! including none.

use nutsh_catalog::Kind;
use nutsh_core::search::Found;

use crate::key::Key;

/// What a key did, for the app to act on, named the way every other modal in this crate names
/// it: the view cannot reach the store or the stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Moved,
    /// `⏎` on a result: open this entity.
    Chose {
        kind: &'static Kind,
        ext_id: String,
    },
    Closed,
    Ignored,
}

/// One line of the list. Headers and hits share an index so one cursor walks the whole thing,
/// the way the sidebar's groups and items do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Group {
        kind: &'static Kind,
        hits: usize,
    },
    Hit {
        kind: &'static Kind,
        ext_id: String,
        name: String,
        detail: String,
    },
}

impl Row {
    fn is_hit(&self) -> bool {
        matches!(self, Row::Hit { .. })
    }
}

#[derive(Debug)]
pub struct Search {
    /// The term as typed, for the box's title.
    pub term: String,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Kinds the store held a table for when the term was run.
    pub loaded: usize,
    /// Kinds in the catalog it held nothing for.
    pub not_loaded: usize,
    /// Kinds this search asked the Prism Central about, and how many have answered. Equal when
    /// the fan-out is done; the title counts up in between.
    pub asked: usize,
    pub answered: usize,
    /// The one-shot subscriptions the fan-out holds, so their answers can be counted and the
    /// rest cancelled when the screen closes.
    pub subs: Vec<nutsh_core::scheduler::SubId>,
}

impl Search {
    /// Flatten what the store answered into one list, with the cursor on the first result.
    /// Rebuild the rows from a fresh walk of the store, keeping the cursor where it was: the
    /// fan-out lands one kind at a time, and a list that jumped under the hand on every answer
    /// would be unusable for the twenty seconds it takes.
    pub fn refill(&mut self, found: Found) {
        let at = self.selected;
        let rebuilt = Search::new(self.term.clone(), found);
        self.rows = rebuilt.rows;
        self.loaded = rebuilt.loaded;
        self.not_loaded = rebuilt.not_loaded;
        self.selected = at.min(self.rows.len().saturating_sub(1));
        if !self.rows.get(self.selected).is_some_and(Row::is_hit) {
            self.selected = self.rows.iter().position(Row::is_hit).unwrap_or(0);
        }
    }

    pub fn new(term: String, found: Found) -> Search {
        let mut rows = Vec::new();
        for group in found.groups {
            rows.push(Row::Group {
                kind: group.kind,
                hits: group.hits.len(),
            });
            for hit in group.hits {
                rows.push(Row::Hit {
                    kind: hit.kind,
                    ext_id: hit.ext_id,
                    name: hit.name,
                    detail: hit.detail,
                });
            }
        }
        // On the first result and not on the first row: a header is a caption, and a list that
        // opened with the cursor on one would need a keypress before `⏎` meant anything.
        let selected = rows.iter().position(Row::is_hit).unwrap_or(0);
        Search {
            term,
            rows,
            selected,
            loaded: found.loaded,
            not_loaded: found.not_loaded,
            asked: 0,
            answered: 0,
            subs: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many results, captions excluded: what the box's `[n]` counts.
    pub fn hits(&self) -> usize {
        self.rows.iter().filter(|r| r.is_hit()).count()
    }

    /// The reach, in the words the box prints under the results. While the fan-out is running
    /// it counts up: a number that is still moving is the honest answer to "is that all of
    /// them", and the old line said `2 loaded` in exactly the voice of a complete one.
    pub fn reach(&self) -> String {
        if self.asked > 0 && self.answered < self.asked {
            return format!(
                "asking {} kinds · {} answered · {} loaded",
                self.asked, self.answered, self.loaded
            );
        }
        if self.asked > 0 {
            return format!(
                "searched {} kind{} · asked the PC for {}",
                self.loaded,
                if self.loaded == 1 { "" } else { "s" },
                self.asked
            );
        }
        format!(
            "searched {} loaded kind{} · {} not loaded",
            self.loaded,
            if self.loaded == 1 { "" } else { "s" },
            self.not_loaded
        )
    }

    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn key(&mut self, key: Key) -> Action {
        let last = self.rows.len().saturating_sub(1);
        let (next, down) = match key {
            Key::Esc | Key::Char('q') => return Action::Closed,
            Key::Enter => {
                return match self.current() {
                    Some(Row::Hit { kind, ext_id, .. }) => Action::Chose {
                        kind,
                        ext_id: ext_id.clone(),
                    },
                    // A caption opens nothing; the kind it names is one keypress away in the
                    // menu, and pretending otherwise would open a table the user did not ask for.
                    Some(Row::Group { .. }) | None => Action::Ignored,
                };
            }
            Key::Char('j') | Key::Down => ((self.selected + 1).min(last), true),
            Key::Char('k') | Key::Up => (self.selected.saturating_sub(1), false),
            Key::Char('g') | Key::Home => (0, true),
            Key::Char('G') | Key::End => (last, false),
            Key::PageDown => ((self.selected + 20).min(last), true),
            Key::PageUp => (self.selected.saturating_sub(20), false),
            _ => return Action::Ignored,
        };
        self.selected = self.past_caption(next, down);
        Action::Moved
    }

    /// A caption is skipped in the direction of travel, so `j` and `k` step between results and
    /// the cursor is never on a row `⏎` would do nothing with. The same rule the sidebar uses
    /// for the separator between its two halves.
    ///
    /// At an end with nothing but captions beyond it the cursor stays where it was, which is
    /// the only honest answer: there is no result in that direction.
    fn past_caption(&self, from: usize, down: bool) -> usize {
        let mut at = from;
        loop {
            match self.rows.get(at) {
                Some(row) if row.is_hit() => return at,
                Some(_) if down && at + 1 < self.rows.len() => at += 1,
                Some(_) if !down && at > 0 => at -= 1,
                // Off an end, or stuck on a caption with nothing past it.
                _ => return self.selected,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nutsh_core::search::{Group, Hit};

    fn kind(id: &str) -> &'static Kind {
        nutsh_catalog::kind(id).expect("the catalog has it")
    }

    fn hit(kind: &'static Kind, name: &str) -> Hit {
        Hit {
            kind,
            ext_id: format!("id-{name}"),
            name: name.to_string(),
            detail: "192.0.2.21".to_string(),
        }
    }

    fn found() -> Found {
        let vm = kind("vmm.ahv.config.Vm");
        let host = kind("clustermgmt.config.Host");
        Found {
            groups: vec![
                Group {
                    kind: vm,
                    hits: vec![hit(vm, "web-01"), hit(vm, "web-02")],
                },
                Group {
                    kind: host,
                    hits: vec![hit(host, "ahv-node-1")],
                },
            ],
            loaded: 14,
            not_loaded: 248,
        }
    }

    /// The cursor opens on a result and only ever lands on one: a caption is a caption.
    #[test]
    fn the_cursor_walks_results_and_steps_over_captions() {
        let mut s = Search::new("192.0.2.21".into(), found());
        assert_eq!(s.rows.len(), 5, "two captions and three results");
        assert_eq!(s.selected, 1, "the first result, not the first caption");
        assert_eq!(s.key(Key::Char('j')), Action::Moved);
        assert_eq!(s.selected, 2);
        assert_eq!(s.key(Key::Char('j')), Action::Moved);
        assert_eq!(s.selected, 4, "over the Hosts caption in one step");
        assert_eq!(s.key(Key::Char('j')), Action::Moved);
        assert_eq!(s.selected, 4, "and nothing below the last result");
        assert_eq!(s.key(Key::Char('k')), Action::Moved);
        assert_eq!(s.selected, 2, "back over it the other way");
        assert_eq!(s.key(Key::Char('g')), Action::Moved);
        assert_eq!(s.selected, 1, "`g` is the first result");
        assert_eq!(s.key(Key::Char('G')), Action::Moved);
        assert_eq!(s.selected, 4);
        assert_eq!(s.key(Key::Char('z')), Action::Ignored);
    }

    /// `⏎` names the entity under the cursor; `esc` and `q` close.
    #[test]
    fn enter_names_the_row_under_the_cursor() {
        let mut s = Search::new("web".into(), found());
        assert_eq!(
            s.key(Key::Enter),
            Action::Chose {
                kind: kind("vmm.ahv.config.Vm"),
                ext_id: "id-web-01".into(),
            }
        );
        assert_eq!(s.key(Key::Esc), Action::Closed);
        assert_eq!(s.key(Key::Char('q')), Action::Closed);
    }

    /// A search that found nothing is still a search that looked somewhere, and the box says
    /// where. Every movement key survives the empty list.
    #[test]
    fn an_empty_result_still_states_its_reach() {
        let mut s = Search::new(
            "nothing".into(),
            Found {
                groups: Vec::new(),
                loaded: 1,
                not_loaded: 261,
            },
        );
        assert!(s.is_empty());
        assert_eq!(s.reach(), "searched 1 loaded kind · 261 not loaded");
        for key in [Key::Char('j'), Key::Char('G'), Key::PageDown, Key::End] {
            assert_eq!(s.key(key), Action::Moved);
            assert_eq!(s.selected, 0);
        }
        assert_eq!(s.key(Key::Enter), Action::Ignored, "nothing to open");
    }

    /// The plural is the count's, not a guess: `1 loaded kind`, `14 loaded kinds`.
    #[test]
    fn the_reach_counts_in_words_that_agree_with_it() {
        let s = Search::new("x".into(), found());
        assert_eq!(s.reach(), "searched 14 loaded kinds · 248 not loaded");
    }
}
