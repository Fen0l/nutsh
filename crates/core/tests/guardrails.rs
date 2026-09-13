use nutsh_catalog::{Action, ConfirmKind, Kind, kind};
use nutsh_config::RuleSpec;
use nutsh_core::can_i::CanI;
use nutsh_core::guardrails::{Guardrails, Rule, Verdict};
use nutsh_core::journal::{Attempt, Journal, JournalId, JournalOutcome};

fn vm() -> &'static Kind {
    kind("vmm.ahv.config.Vm").unwrap()
}

fn act(name: &str) -> &'static Action {
    vm().action(name).unwrap()
}

fn spec(toml: &str) -> Vec<RuleSpec> {
    toml::from_str::<nutsh_config::Config>(toml)
        .unwrap()
        .guardrails
}

fn rails(toml: &str) -> Guardrails {
    Guardrails::from_config(false, &spec(toml)).unwrap()
}

#[test]
fn a_deny_rule_matches_on_globs_over_context_kind_and_action() {
    let g = rails(
        r#"
[[guardrails]]
contexts = ["*prod*"]
kinds    = ["vm", "cluster"]
actions  = ["delete", "power-off"]
deny     = true
reason   = "Prod changes go through change control."
"#,
    );
    assert_eq!(
        g.verdict(
            "eu-prod-1",
            vm(),
            act("power-off"),
            1,
            &CanI::Unknown("x".into())
        ),
        Verdict::Deny(Some("Prod changes go through change control.".into()))
    );
    // `kinds` matches the id, any alias, or the display name. A rule without a reason denies
    // with `None`: the wording is `actions::reason`'s, not this module's.
    let g2 = rails("[[guardrails]]\nkinds = [\"vmm.ahv.config.Vm\"]\ndeny = true\n");
    assert_eq!(
        g2.verdict("lab", vm(), act("power-on"), 1, &CanI::Unknown("x".into())),
        Verdict::Deny(None)
    );
    let g3 = rails("[[guardrails]]\nkinds = [\"Virtual Machines\"]\ndeny = true\n");
    assert!(matches!(
        g3.verdict("lab", vm(), act("power-on"), 1, &CanI::Unknown("x".into())),
        Verdict::Deny(_)
    ));
    // A different context, kind, or action is untouched.
    assert!(!matches!(
        rails("[[guardrails]]\ncontexts = [\"prod\"]\ndeny = true\n").verdict(
            "lab",
            vm(),
            act("power-off"),
            1,
            &CanI::Unknown("x".into())
        ),
        Verdict::Deny(_)
    ));
}

#[test]
fn read_only_overrides_everything_and_can_i_is_answered_before_the_bulk_cap() {
    let ro = Guardrails::from_config(true, &[]).unwrap();
    assert_eq!(
        ro.verdict(
            "lab",
            vm(),
            act("power-on"),
            1,
            &CanI::Yes {
                via: "Admin".into()
            }
        ),
        Verdict::ReadOnly
    );
    let g = rails("");
    // Ten marked, and the account may not do it at all: "you may not" is the more useful answer.
    let no = CanI::No {
        missing: act("power-off").roles,
    };
    assert_eq!(
        g.verdict("lab", vm(), act("power-off"), 10, &no),
        Verdict::NotPermitted(act("power-off").roles)
    );
    // Unknown never denies.
    assert_eq!(
        g.verdict(
            "lab",
            vm(),
            act("power-off"),
            10,
            &CanI::Unknown("iam not served".into())
        ),
        Verdict::TooMany { max: 5 }
    );
}

#[test]
fn confirm_is_the_stronger_of_the_action_and_the_rule_and_max_bulk_resolves_on_its_own() {
    let yes = CanI::Yes {
        via: "Admin".into(),
    };
    // A rule that sets only `confirm` must not silently drop the built-in bulk cap.
    let g = rails("[[guardrails]]\nactions = [\"power-on\"]\nconfirm = \"yes\"\n");
    assert_eq!(
        g.verdict("lab", vm(), act("power-on"), 1, &yes),
        Verdict::Confirm(ConfirmKind::Yes)
    );
    assert_eq!(
        g.verdict("lab", vm(), act("power-on"), 9, &yes),
        Verdict::TooMany { max: 5 }
    );
    // The action's own floor wins when it is stronger than the rule's. The floor is set on a
    // copy of `delete` rather than read from the catalog, so this holds whether or not the
    // curation that raises the catalog's `delete` to `type-name` has landed.
    let weak = rails("[[guardrails]]\nactions = [\"delete\"]\nconfirm = \"none\"\n");
    let delete = Action {
        confirm: ConfirmKind::TypeName,
        ..*act("delete")
    };
    assert_eq!(
        weak.verdict("lab", vm(), &delete, 1, &yes),
        Verdict::Confirm(ConfirmKind::TypeName)
    );
    // A user rule beats the built-in one, because the built-ins are appended after it.
    let one = rails("[[guardrails]]\nactions = [\"*\"]\nmax_bulk = 1\n");
    assert_eq!(
        one.verdict("lab", vm(), act("power-on"), 2, &yes),
        Verdict::TooMany { max: 1 }
    );
    // Built-ins: delete asks for the name, power-off asks, everything else runs.
    let plain = rails("");
    assert_eq!(
        plain.verdict("lab", vm(), act("delete"), 1, &yes),
        Verdict::Confirm(ConfirmKind::TypeName)
    );
    assert_eq!(
        plain.verdict("lab", vm(), act("power-off"), 1, &yes),
        Verdict::Confirm(ConfirmKind::Yes)
    );
    assert_eq!(
        plain.verdict("lab", vm(), act("power-on"), 1, &yes),
        Verdict::Allow
    );
}

/// `Guardrails::default()` is what the TUI's session builder falls back to, so it must be the
/// same thing an empty file gives: not read-only, and every built-in.
#[test]
fn default_is_an_empty_config_files_guardrails() {
    let d = Guardrails::default();
    assert!(!d.readonly);
    assert_eq!(d.rules, Guardrails::from_config(false, &[]).unwrap().rules);
}

#[test]
fn rule_from_spec_reads_the_three_confirm_spellings_and_names_a_fourth() {
    for (text, want) in [
        ("none", ConfirmKind::None),
        ("yes", ConfirmKind::Yes),
        ("type-name", ConfirmKind::TypeName),
    ] {
        let s = spec(&format!("[[guardrails]]\nconfirm = \"{text}\"\n"));
        assert_eq!(Rule::from_spec(&s[0]).unwrap().confirm, Some(want));
    }
    let s = spec("[[guardrails]]\nconfirm = \"maybe\"\n");
    let err = Rule::from_spec(&s[0]).unwrap_err();
    assert!(err.contains("maybe"), "{err}");
    assert!(err.contains("type-name"), "{err}");
    // The two settings that would parse and then do something other than what they say.
    let s = spec("[[guardrails]]\nmax_bulk = 0\n");
    let err = Rule::from_spec(&s[0]).unwrap_err();
    assert!(err.contains("max_bulk"), "{err}");
    let s = spec("[[guardrails]]\nreason = \"no\"\n");
    let err = Rule::from_spec(&s[0]).unwrap_err();
    assert!(err.contains("deny"), "{err}");
    // With several tables, the error says which one.
    let err = Guardrails::from_config(
        false,
        &spec("[[guardrails]]\ndeny = true\n\n[[guardrails]]\nconfirm = \"maybe\"\n"),
    )
    .unwrap_err();
    assert!(err.starts_with("guardrails[1]: "), "{err}");
}

#[test]
fn an_unknown_guardrail_key_is_a_parse_error_naming_it() {
    // `deny_unknown_fields` on `Config` does not descend into a nested struct, so `RuleSpec`
    // carries its own: `confrim = "yes"` parsing and silently doing nothing is the worst
    // outcome a safety feature can have.
    let err = toml::from_str::<nutsh_config::Config>("[[guardrails]]\nconfrim = \"yes\"\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("confrim"), "{err}");
}

#[test]
fn the_journal_records_identifiers_and_settles_in_place() {
    use std::time::{Duration, UNIX_EPOCH};

    let at = UNIX_EPOCH + Duration::from_secs(1_788_602_400);
    let attempt = |ext_id: &'static str, name: &'static str, action: &'static str| Attempt {
        context: "lab",
        kind: vm(),
        ext_id,
        name,
        action,
    };
    let mut j = Journal::default();
    let id = j.record(
        attempt("3d0c", "web-01", "power-off"),
        at,
        JournalOutcome::Started,
    );
    assert_eq!(j.entries().len(), 1);
    j.settle(id, JournalOutcome::Succeeded, Some("ZXJnb24=:abc".into()));
    assert_eq!(j.entries().len(), 1, "updated in place, not appended");
    let e = j.entries().next().unwrap();
    assert_eq!(e.outcome, JournalOutcome::Succeeded);
    assert_eq!(e.task_ext_id.as_deref(), Some("ZXJnb24=:abc"));
    assert_eq!(e.name, "web-01");
    assert_eq!(e.ext_id, "3d0c");
    assert_eq!(e.kind.id, "vmm.ahv.config.Vm");
    assert_eq!(e.action, "power-off");
    // Capacity 500, oldest dropped, newest kept, and ids stay valid across the eviction.
    let mut j = Journal::default();
    let first = j.record(attempt("1", "a", "power-off"), at, JournalOutcome::Started);
    let names: Vec<String> = (0..600).map(|i| format!("v{i}")).collect();
    for name in &names {
        j.record(
            Attempt {
                name,
                ..attempt("x", "", "power-on")
            },
            at,
            JournalOutcome::Started,
        );
    }
    assert_eq!(j.entries().len(), 500);
    assert!(j.entries().any(|e| e.name == "v599"));
    j.settle(first, JournalOutcome::Succeeded, None);
    assert!(
        j.entries().all(|e| e.name != "a"),
        "settling an evicted id is a no-op, never a panic"
    );
    // `JournalId::default()` is what a `TaskWatch` carries when its attempt was never
    // journalled. It has to match no entry - `next` starts at one - or settling that watch
    // would overwrite the outcome and task of the session's first real attempt.
    let mut j = Journal::default();
    let first = j.record(
        attempt("3d0c", "web-01", "power-off"),
        at,
        JournalOutcome::Started,
    );
    assert_ne!(first, JournalId::default());
    j.settle(
        JournalId::default(),
        JournalOutcome::Succeeded,
        Some("ZXJnb24=:abc".into()),
    );
    let e = j.entries().next().unwrap();
    assert_eq!(e.outcome, JournalOutcome::Started, "untouched");
    assert_eq!(e.task_ext_id, None);
}

/// The label is what the journal view prints, once per visible row per frame; a denial is
/// already worded by `actions::reason` and must not be prefixed again.
#[test]
fn outcome_labels_borrow_the_constants_and_do_not_prefix_a_denial_twice() {
    assert_eq!(JournalOutcome::Started.label(), "started");
    assert_eq!(JournalOutcome::Succeeded.label(), "succeeded");
    assert_eq!(
        JournalOutcome::Failed("boom".into()).label(),
        "failed: boom"
    );
    assert_eq!(
        JournalOutcome::Denied("denied: prod".into()).label(),
        "denied: prod"
    );
    // A cancellation has its own word: `unknown: cancelled` would contradict itself.
    assert_eq!(JournalOutcome::Cancelled.label(), "cancelled");
    assert_eq!(
        JournalOutcome::Unknown("still running after 15m".into()).label(),
        "unknown: still running after 15m"
    );
}
