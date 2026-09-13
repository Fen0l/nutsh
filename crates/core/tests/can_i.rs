mod common;

use nutsh_core::can_i::{CanI, CanIndex, resolve};
use nutsh_mockpc::MockPc;
use serde_json::json;

fn vm_power_off() -> &'static nutsh_catalog::Action {
    nutsh_catalog::kind("vmm.ahv.config.Vm")
        .unwrap()
        .action("power-off")
        .unwrap()
}

#[test]
fn roles_intersect_the_operations_role_list() {
    let index = CanIndex::with_roles(vec!["Virtual Machine Admin".into()]);
    assert_eq!(
        index.can(vm_power_off()),
        CanI::Yes {
            via: "Virtual Machine Admin".into()
        }
    );
    let index = CanIndex::with_roles(vec!["Reader".into()]);
    assert_eq!(
        index.can(vm_power_off()),
        CanI::No {
            missing: vm_power_off().roles
        }
    );
    // Case-insensitively, because a directory-sourced role name is whatever it is.
    let index = CanIndex::with_roles(vec!["super admin".into()]);
    assert!(matches!(index.can(vm_power_off()), CanI::Yes { .. }));
}

#[test]
fn unknown_is_a_first_class_answer_and_never_hides_anything() {
    let unresolved = CanIndex::unknown("cannot read authorization policies (HTTP 403)");
    assert_eq!(
        unresolved.can(vm_power_off()),
        CanI::Unknown("cannot read authorization policies (HTTP 403)".into())
    );
    // An operation the spec lists no roles for cannot be answered either.
    let no_roles = nutsh_catalog::KINDS
        .iter()
        .flat_map(|k| k.actions.iter())
        .find(|a| a.roles.is_empty())
        .expect("the specs omit x-permissions somewhere");
    let index = CanIndex::with_roles(vec!["Super Admin".into()]);
    assert_eq!(
        index.can(no_roles),
        CanI::Unknown("the spec lists no roles for this operation".into())
    );
}

/// `AuthorizationPolicy.identities[]` is `{identityFilter: <free-form object>}` with no declared
/// sub-structure in any IAM spec version, so `resolve` walks the runtime JSON for string leaves.
#[test]
fn the_identity_filter_leaf_walk_matches_a_user_or_a_group() {
    use nutsh_core::can_i::policy_matches;
    let policy = json!({
        "role": "role-1",
        "identities": [
            {"identityFilter": {"user": {"uuid": {"anyof": ["u-1"]}}}},
            {"identityFilter": {"group": {"name": {"anyof": ["Admins"]}}}}
        ]
    });
    assert!(policy_matches(
        &policy,
        "u-1",
        "admin",
        &["g-9".into()],
        &["Others".into()]
    ));
    assert!(policy_matches(
        &policy,
        "u-2",
        "admin",
        &["g-9".into()],
        &["admins".into()]
    ));
    assert!(!policy_matches(
        &policy,
        "u-2",
        "bob",
        &["g-9".into()],
        &["Others".into()]
    ));
    // A policy matching nothing is skipped, never treated as a match.
    assert!(!policy_matches(
        &json!({"role": "r", "identities": []}),
        "u-1",
        "admin",
        &[],
        &[]
    ));
    // An entity without an extId carries "", which must not pair with an empty leaf.
    let empty_leaf = json!({
        "role": "r",
        "identities": [{"identityFilter": {"user": {"uuid": {"anyof": [""]}}}}]
    });
    assert!(!policy_matches(&empty_leaf, "", "admin", &[], &[]));
    assert!(!policy_matches(
        &empty_leaf,
        "u-1",
        "admin",
        &["".into()],
        &["".into()]
    ));
}

#[test]
fn the_cell_resolves_once_and_can_be_invalidated() {
    use nutsh_core::can_i::CanICell;
    let cell = CanICell::default();
    assert!(matches!(cell.get().can(vm_power_off()), CanI::Unknown(_)));
    let generation = cell
        .claim()
        .expect("the first caller starts the resolution");
    assert!(cell.claim().is_none(), "and nobody else does");
    cell.set(generation, CanIndex::with_roles(vec!["Super Admin".into()]));
    assert!(matches!(cell.get().can(vm_power_off()), CanI::Yes { .. }));
    cell.invalidate();
    assert!(
        matches!(cell.get().can(vm_power_off()), CanI::Unknown(_)),
        "an invalidated cell answers Unknown until re-resolved"
    );
    assert!(
        cell.claim().is_some(),
        "a 403 makes the next menu re-resolve"
    );
}

/// The action 403s while the four IAM lists are still in flight: the resolution that started
/// before the 403 must not overwrite the reset with the answer the 403 just disproved.
#[test]
fn the_cell_drops_a_resolution_from_an_invalidated_generation() {
    use nutsh_core::can_i::CanICell;
    let cell = CanICell::default();
    let stale = cell.claim().expect("first claim");
    cell.invalidate();
    cell.set(stale, CanIndex::with_roles(vec!["Super Admin".into()]));
    assert!(
        matches!(cell.get().can(vm_power_off()), CanI::Unknown(_)),
        "a stale resolution is dropped"
    );
    let fresh = cell.claim().expect("the reset lets the next caller claim");
    assert_ne!(fresh, stale);
    cell.set(fresh, CanIndex::with_roles(vec!["Super Admin".into()]));
    assert!(matches!(cell.get().can(vm_power_off()), CanI::Yes { .. }));
}

/// The roles are what is persisted, never the resolved verdict: `with_roles` computes
/// `can()` from the roles *plus* the catalog's policies, so a persisted verdict would rot
/// the first time a policy or an action id changed.
#[test]
fn a_restored_index_answers_and_still_lets_the_first_claim_re_resolve() {
    use nutsh_core::can_i::CanICell;
    let cell = CanICell::default();
    assert!(cell.get().roles().is_none(), "nothing known yet");
    cell.restore(CanIndex::with_roles(vec!["Prism Admin".into()]));
    assert_eq!(
        cell.get().roles().map(<[String]>::to_vec),
        Some(vec!["Prism Admin".to_string()])
    );
    assert!(
        cell.claim().is_some(),
        "a restore paints the menu; it does not stand in for a resolution"
    );
}

/// A head start, never a rewind: a restore that arrives after the resolution has been claimed
/// is dropped. Reinstating the cache's roles there would stand for the whole session, because
/// `claimed` is already true and nobody would re-resolve.
#[test]
fn a_restore_never_replaces_a_claimed_resolution() {
    use nutsh_core::can_i::CanICell;
    let cell = CanICell::default();
    let generation = cell.claim().expect("first claim");
    cell.set(generation, CanIndex::with_roles(vec!["Prism Admin".into()]));
    cell.restore(CanIndex::with_roles(vec!["Prism Viewer".into()]));
    assert_eq!(
        cell.get().roles().map(<[String]>::to_vec),
        Some(vec!["Prism Admin".to_string()]),
        "the resolution is the authority; the cache's roles are behind it"
    );
}

/// The end-to-end path on a fixture tree with all four IAM lists: the case-insensitive
/// username match, the group-name route and the user-uuid route through `identityFilter`,
/// the role-id-to-`displayName` hop, and one entry per role however many policies grant it.
#[tokio::test]
async fn resolve_walks_the_four_lists_and_reports_each_role_once() {
    let pc = MockPc::builder()
        .fixtures(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
        .start()
        .await;
    let index = resolve(&common::client(&pc).await, "admin").await;
    assert_eq!(
        index.roles(),
        Some(&["Virtual Machine Admin".to_string()][..]),
        "two policies grant r-1 to admin (by group, by uuid); bob's Super Admin is not admin's"
    );
    assert_eq!(
        index.can(vm_power_off()),
        CanI::Yes {
            via: "Virtual Machine Admin".into()
        }
    );
    for path in [
        "/iam/v4.0/authn/users",
        "/iam/v4.0/authn/user-groups",
        "/iam/v4.0/authz/authorization-policies",
        "/iam/v4.0/authz/roles",
    ] {
        assert!(!pc.requests_to(path).is_empty(), "{path} was never listed");
    }
    // IAM v4.0 lists groups but not their members, so a group-granted role counts for every
    // user: bob holds Super Admin through his own policy and Virtual Machine Admin through the
    // Admins group he may or may not be in. Erring towards Yes is the point: a false Yes costs
    // one 403, a false No would hide the action.
    let index = resolve(&common::client(&pc).await, "BOB").await;
    assert_eq!(
        index.roles(),
        Some(
            &[
                "Super Admin".to_string(),
                "Virtual Machine Admin".to_string()
            ][..]
        )
    );
}

/// The bundled fixtures carry no IAM lists, so the mock answers every one with an empty page:
/// the account is then not in the user list, which is an `Unknown` with that reason.
#[tokio::test]
async fn resolve_says_why_when_the_user_list_lacks_the_account() {
    let pc = MockPc::builder().start().await;
    let index = resolve(&common::client(&pc).await, "admin").await;
    assert_eq!(index.roles(), None);
    assert_eq!(
        index.can(vm_power_off()),
        CanI::Unknown("admin is not in the IAM user list".into())
    );
}

/// A 403 on the first list is an `Unknown` carrying the HTTP reason, never a `No`.
#[tokio::test]
async fn resolve_reports_a_forbidden_iam_as_unknown() {
    let pc = MockPc::builder().forbid_namespace("iam").start().await;
    let index = resolve(&common::client(&pc).await, "admin").await;
    let CanI::Unknown(why) = index.can(vm_power_off()) else {
        panic!("a forbidden IAM must be Unknown");
    };
    assert!(
        why.starts_with("cannot read users (forbidden (HTTP 403)"),
        "{why}"
    );
}
