//! Local rules that can refuse a mutation before it is sent. `Verdict` carries facts;
//! `core::actions::reason` does the wording, so no variant holds a string another arm would
//! prefix again.

use nutsh_catalog::{Action, ConfirmKind, Kind};
use nutsh_config::RuleSpec;

use crate::can_i::CanI;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Confirm(ConfirmKind),
    /// The session is read-only: `--readonly`, the context's, or the file's.
    ReadOnly,
    /// A denying rule matched, with its `reason` if it gave one.
    Deny(Option<String>),
    /// can-i said No; the roles it wants.
    NotPermitted(&'static [&'static str]),
    TooMany {
        max: usize,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    pub contexts: Vec<String>,
    pub kinds: Vec<String>,
    pub actions: Vec<String>,
    pub deny: bool,
    pub reason: Option<String>,
    pub confirm: Option<ConfirmKind>,
    pub max_bulk: Option<usize>,
}

impl Rule {
    /// The one place `confirm`'s three spellings become a `ConfirmKind`. Refuses the two
    /// settings that parse and then do something other than what they say: a cap of zero is a
    /// deny with a worse message, and a reason on a rule that does not deny is never shown.
    pub fn from_spec(spec: &RuleSpec) -> Result<Rule, String> {
        let confirm = match spec.confirm.as_deref() {
            None => None,
            Some("none") => Some(ConfirmKind::None),
            Some("yes") => Some(ConfirmKind::Yes),
            Some("type-name") => Some(ConfirmKind::TypeName),
            Some(other) => {
                return Err(format!(
                    "confirm {other:?} is not one of none, yes, type-name"
                ));
            }
        };
        if spec.max_bulk == Some(0) {
            return Err("max_bulk must be at least 1".to_string());
        }
        if spec.reason.is_some() && !spec.deny {
            return Err("reason needs deny = true".to_string());
        }
        Ok(Rule {
            contexts: spec.contexts.clone(),
            kinds: spec.kinds.clone(),
            actions: spec.actions.clone(),
            deny: spec.deny,
            reason: spec.reason.clone(),
            confirm,
            max_bulk: spec.max_bulk,
        })
    }

    fn matches(&self, context: &str, kind: &Kind, action: &Action) -> bool {
        any(&self.contexts, |g| glob(g, context))
            && any(&self.kinds, |g| {
                glob(g, kind.id) || glob(g, kind.display) || kind.aliases.iter().any(|a| glob(g, a))
            })
            && any(&self.actions, |g| glob(g, action.name))
    }
}

/// An absent list is `["*"]`.
fn any(globs: &[String], f: impl Fn(&str) -> bool) -> bool {
    globs.is_empty() || globs.iter().any(|g| f(g))
}

/// `*` and `?` over ASCII, compared case-insensitively. Iterative, with one backtrack point at
/// the last `*`, so a pattern with many stars stays linear in the text rather than exponential:
/// the inputs are short names today, and nothing enforces that they stay short.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    // The last `*` seen: the pattern index after it, and the text index it has swallowed up to.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some(b'*') => {
                star = Some((pi + 1, ti));
                pi += 1;
            }
            Some(b'?') => {
                pi += 1;
                ti += 1;
            }
            Some(c) if c.eq_ignore_ascii_case(&t[ti]) => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                // Let the last `*` swallow one more byte and try again from after it.
                Some((after, from)) => {
                    star = Some((after, from + 1));
                    pi = after;
                    ti = from + 1;
                }
                None => return false,
            },
        }
    }
    // Trailing stars match nothing.
    while p.get(pi) == Some(&b'*') {
        pi += 1;
    }
    pi == p.len()
}

/// Marked rows one action may touch at once when no rule says otherwise.
pub const DEFAULT_MAX_BULK: usize = 5;

#[derive(Debug, Clone)]
pub struct Guardrails {
    pub readonly: bool,
    pub rules: Vec<Rule>,
}

/// What an empty config file gives: not read-only, and the built-ins. Not derived, because a
/// derived `Default` would carry no rules at all, and a `delete` that runs without asking for
/// the name is exactly the silent failure a safety feature must not have.
impl Default for Guardrails {
    fn default() -> Self {
        Guardrails {
            readonly: false,
            rules: built_in(),
        }
    }
}

impl Guardrails {
    /// The user's rules, then the built-ins, so a user rule always wins. An error names the
    /// table by its position, so the value is found without grepping every rule for it.
    pub fn from_config(readonly: bool, specs: &[RuleSpec]) -> Result<Guardrails, String> {
        let mut rules = specs
            .iter()
            .enumerate()
            .map(|(i, s)| Rule::from_spec(s).map_err(|e| format!("guardrails[{i}]: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        rules.extend(built_in());
        Ok(Guardrails { readonly, rules })
    }

    /// In order: read-only, the first denying rule, can-i, the bulk cap, the confirm floor.
    /// can-i is answered before the bulk cap because "you may not do this at all" is more
    /// useful than "not to seven of them".
    pub fn verdict(
        &self,
        context: &str,
        kind: &Kind,
        action: &Action,
        count: usize,
        can_i: &CanI,
    ) -> Verdict {
        if self.readonly {
            return Verdict::ReadOnly;
        }
        // Three passes over a handful of rules rather than one collected `Vec`: this may run
        // per frame for every action the palette greys out, and the draw loop does not allocate.
        let matching = || {
            self.rules
                .iter()
                .filter(|r| r.matches(context, kind, action))
        };
        if let Some(denied) = matching().find(|r| r.deny) {
            return Verdict::Deny(denied.reason.clone());
        }
        if let CanI::No { missing } = can_i {
            return Verdict::NotPermitted(missing);
        }
        // Resolved on its own, so a rule that sets only `confirm` does not drop the cap.
        let max = matching()
            .find_map(|r| r.max_bulk)
            .unwrap_or(DEFAULT_MAX_BULK);
        if count > max {
            return Verdict::TooMany { max };
        }
        let floor = matching()
            .find_map(|r| r.confirm)
            .unwrap_or(ConfirmKind::None)
            .max(action.confirm);
        match floor {
            ConfirmKind::None => Verdict::Allow,
            k => Verdict::Confirm(k),
        }
    }
}

/// The same `Rule` values a config file would carry, built in code, so there is one matcher and
/// one set of tests.
fn built_in() -> Vec<Rule> {
    let one = |actions: &[&str], confirm: ConfirmKind| Rule {
        actions: actions.iter().map(|a| (*a).to_string()).collect(),
        confirm: Some(confirm),
        ..Rule::default()
    };
    vec![
        one(&["delete"], ConfirmKind::TypeName),
        one(
            &[
                "power-off",
                "power-cycle",
                "reset",
                "reboot",
                "shutdown",
                "guest-shutdown",
                "guest-reboot",
                "revert",
                "enter-host-maintenance",
            ],
            ConfirmKind::Yes,
        ),
        Rule {
            max_bulk: Some(DEFAULT_MAX_BULK),
            ..Rule::default()
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::glob;

    /// Every `deny` rule hangs on this matcher, and a miss here is the fail-open the module
    /// exists to prevent, so each operator is pinned on its own.
    #[test]
    fn glob_matches_star_question_mark_and_case_insensitively() {
        for (p, t, want) in [
            ("*", "", true),
            ("", "", true),
            ("", "a", false),
            ("*", "abc", true),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("?", "", false),
            ("VM", "vm", true),
            ("virtual*", "Virtual Machines", true),
            ("a*", "a", true),
            ("*maintenance*", "enter-host-maintenance", true),
            ("prod", "eu-prod-1", false),
            ("*prod*", "eu-prod-1", true),
            ("a*", "b", false),
            ("a*c", "abbc", true),
            ("a*c", "abcb", false),
            ("abc", "ab", false),
            // Many stars against a run of the same byte: linear, and still a miss.
            (
                "*a*a*a*a*a*a*a*a*b",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                false,
            ),
            (
                "*a*a*a*a*a*a*a*a*b",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
                true,
            ),
        ] {
            assert_eq!(glob(p, t), want, "{p:?} vs {t:?}");
        }
    }
}
