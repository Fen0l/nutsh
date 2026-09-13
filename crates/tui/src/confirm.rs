//! The confirm dialog. A `TypeName` confirm over marks cannot ask for seven names, so it asks
//! for one deliberate phrase instead.

use nutsh_catalog::ConfirmKind;

use crate::key::Key;

/// What a key did, for the app to act on. `Event`, not `Action`, for the reason
/// [`crate::menu::Event`] is: `nutsh_catalog::Action` is what is being confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Run,
    Cancelled,
    Typed,
    Mismatch,
    Ignored,
}

pub struct Confirm {
    pub prompt: String,
    /// The first five names, for a bulk confirm; empty for one row, whose name the prompt
    /// already carries.
    pub names: Vec<String>,
    pub more: usize,
    /// `None` for a yes/no confirm; the literal to match for a type-name one.
    pub expect: Option<String>,
    pub input: String,
}

/// The affected rows a bulk confirm lists by name before it says how many more there are.
const LISTED: usize = 5;

impl Confirm {
    /// `label` is the action's title, `names` every affected row in table order.
    pub fn open(kind: ConfirmKind, label: &str, display: &str, names: &[String]) -> Confirm {
        let one = names.len() == 1;
        let subject = if one {
            names[0].clone()
        } else {
            format!("{} {display}", names.len())
        };
        let expect = match kind {
            ConfirmKind::TypeName if one => Some(names[0].clone()),
            ConfirmKind::TypeName => Some(format!("DELETE {}", names.len())),
            _ => None,
        };
        let prompt = match (&expect, one) {
            (None, _) => format!("{label} {subject}? [y/N]"),
            (Some(_), true) => format!("{label} {subject}. Type the name to confirm:"),
            (Some(what), false) => format!("{label} {subject}. Type {what} to confirm:"),
        };
        Confirm {
            // One name is in the prompt already; listing it under itself says it twice.
            names: if one {
                Vec::new()
            } else {
                names.iter().take(LISTED).cloned().collect()
            },
            more: names.len().saturating_sub(LISTED),
            prompt,
            expect,
            input: String::new(),
        }
    }

    pub fn key(&mut self, key: Key) -> Event {
        match (&self.expect, key) {
            (None, Key::Char('y') | Key::Char('Y')) => Event::Run,
            (None, _) => Event::Cancelled,
            (Some(_), Key::Esc) => Event::Cancelled,
            (Some(want), Key::Enter) => {
                if &self.input == want {
                    Event::Run
                } else {
                    Event::Mismatch
                }
            }
            (Some(_), Key::Char(c)) => {
                self.input.push(c);
                Event::Typed
            }
            (Some(_), Key::Backspace) => {
                self.input.pop();
                Event::Typed
            }
            _ => Event::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("vm-{i:03}")).collect()
    }

    /// A yes/no confirm runs on `y` and cancels on everything else, so a hand on the wrong key
    /// never powers a machine off.
    #[test]
    fn a_yes_confirm_runs_only_on_y() {
        let mut c = Confirm::open(ConfirmKind::Yes, "Power off", "Virtual Machines", &names(1));
        assert_eq!(c.prompt, "Power off vm-000? [y/N]");
        assert!(c.names.is_empty(), "one name is in the prompt already");
        assert_eq!(c.key(Key::Char('Y')), Event::Run);
        assert_eq!(c.key(Key::Char('n')), Event::Cancelled);
        assert_eq!(c.key(Key::Enter), Event::Cancelled);
    }

    #[test]
    fn one_row_types_its_name_and_a_mismatch_does_not_run() {
        let mut c = Confirm::open(
            ConfirmKind::TypeName,
            "Delete",
            "Virtual Machines",
            &names(1),
        );
        assert_eq!(c.prompt, "Delete vm-000. Type the name to confirm:");
        for ch in "vm-00X".chars() {
            assert_eq!(c.key(Key::Char(ch)), Event::Typed);
        }
        assert_eq!(c.key(Key::Enter), Event::Mismatch);
        assert_eq!(c.key(Key::Backspace), Event::Typed);
        assert_eq!(c.key(Key::Char('0')), Event::Typed);
        assert_eq!(c.key(Key::Enter), Event::Run);
    }

    /// Seven names is not a prompt anyone reads, so a bulk confirm asks for one phrase and
    /// lists five of the rows it would touch.
    #[test]
    fn a_bulk_confirm_asks_for_a_phrase_and_lists_five() {
        let mut c = Confirm::open(
            ConfirmKind::TypeName,
            "Delete",
            "Virtual Machines",
            &names(7),
        );
        assert_eq!(
            c.prompt,
            "Delete 7 Virtual Machines. Type DELETE 7 to confirm:"
        );
        assert_eq!(c.names.len(), 5);
        assert_eq!(c.more, 2);
        for ch in "DELETE 7".chars() {
            c.key(Key::Char(ch));
        }
        assert_eq!(c.key(Key::Enter), Event::Run);
        assert_eq!(c.key(Key::Esc), Event::Cancelled);
    }
}
