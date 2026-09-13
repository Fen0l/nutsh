//! Whether this account may perform an action.
//!
//! There is no permission-check endpoint: the answer is assembled from `/iam/v4.0/authn/users`,
//! `/authn/user-groups`, `/authz/authorization-policies` and `/authz/roles`, intersected at the
//! role-name level with the action's `x-permissions.roleList`. Resolution is lazy and cached,
//! and every failure is an `Unknown` with its reason - never a `No`, because nothing here may
//! hide an action the account can in fact perform.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use nutsh_catalog::Action;
use nutsh_prism::{Client, ListOptions, PrismError};
use serde_json::Value;

/// Whether this account may perform an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanI {
    /// One of my roles is in the operation's role list; `via` names it.
    Yes { via: String },
    /// Every role the spec accepts for the operation; none of mine is among them.
    No { missing: &'static [&'static str] },
    /// Not answerable, and why: the lists are still loading, one of them could not be read,
    /// the account is not in the user list, or the spec names no roles for the operation.
    Unknown(String),
}

#[derive(Debug, Clone, Default)]
pub struct CanIndex {
    roles: Option<Vec<String>>,
    why: Option<String>,
}

impl CanIndex {
    pub fn with_roles(roles: Vec<String>) -> CanIndex {
        CanIndex {
            roles: Some(roles),
            why: None,
        }
    }

    pub fn unknown(why: impl Into<String>) -> CanIndex {
        CanIndex {
            roles: None,
            why: Some(why.into()),
        }
    }

    pub fn can(&self, action: &Action) -> CanI {
        let Some(roles) = &self.roles else {
            return CanI::Unknown(
                self.why
                    .clone()
                    .unwrap_or_else(|| "resolving authorization policies".to_string()),
            );
        };
        if action.roles.is_empty() {
            return CanI::Unknown("the spec lists no roles for this operation".to_string());
        }
        match roles
            .iter()
            .find(|mine| action.roles.iter().any(|r| r.eq_ignore_ascii_case(mine)))
        {
            Some(via) => CanI::Yes { via: via.clone() },
            None => CanI::No {
                missing: action.roles,
            },
        }
    }

    /// The roles this session resolved, sorted and each once, for `:can-i`'s own reporting
    /// and for the cache. `None` for an index that could not be resolved, which is never
    /// persisted.
    pub fn roles(&self) -> Option<&[String]> {
        self.roles.as_deref()
    }
}

/// The session's answer, resolved in the background and read from the UI thread. A shared cell
/// rather than a fourth `select!` arm: the event loop already redraws on its one-second tick, so
/// the answer lands within a second of arriving and `run.rs` needs no change.
///
/// Every resolution carries the generation it was claimed under, and `invalidate` opens a new
/// one: a resolution still in flight when a 403 disproved it lands on a dead generation and is
/// dropped, rather than overwriting the reset with the answer the 403 just refuted.
#[derive(Debug, Clone, Default)]
pub struct CanICell {
    inner: Arc<Mutex<Slot>>,
}

#[derive(Debug, Default)]
struct Slot {
    index: CanIndex,
    claimed: bool,
    generation: u64,
}

impl CanICell {
    /// The generation to resolve for, when this caller is the one that should start the
    /// resolution. Idempotent: only the first caller after a `default` or an `invalidate` gets
    /// `Some`.
    pub fn claim(&self) -> Option<u64> {
        let mut guard = self.inner.lock().expect("can-i lock");
        if guard.claimed {
            return None;
        }
        guard.claimed = true;
        Some(guard.generation)
    }

    pub fn get(&self) -> CanIndex {
        self.inner.lock().expect("can-i lock").index.clone()
    }

    /// Stores a resolution claimed under `generation`. One from an invalidated generation is
    /// dropped: the reset, or the claim that followed it, is the authority now.
    pub fn set(&self, generation: u64, index: CanIndex) {
        let mut guard = self.inner.lock().expect("can-i lock");
        if guard.generation == generation {
            guard.index = index;
        }
    }

    /// Paint the menu from roles the cache held, **without** claiming: a restore is a head
    /// start, not a resolution, so the first caller still starts the background re-resolve and
    /// a stale allow the PC then refuses shows its 403 in the flash exactly as a revocation
    /// does today.
    ///
    /// A head start only, in code as well as in prose: once someone has claimed the
    /// resolution, that answer is the authority and the cache's roles are already behind it,
    /// so a late restore is dropped rather than reinstating them for the whole session.
    pub fn restore(&self, index: CanIndex) {
        let mut guard = self.inner.lock().expect("can-i lock");
        if guard.claimed {
            return;
        }
        guard.index = index;
    }

    /// A 403 from an action, or `ctrl-r` with the menu open: the next caller re-resolves, and
    /// whatever the previous generation still delivers is ignored.
    pub fn invalidate(&self) {
        let mut guard = self.inner.lock().expect("can-i lock");
        *guard = Slot {
            index: CanIndex::default(),
            claimed: false,
            generation: guard.generation + 1,
        };
    }
}

/// Four list calls and one intersection. Every failure is an `Unknown` with its reason: `iam`
/// not served, a 403 on any list, a 404 (the catalog knows `iam` only at v4.0, and a PC serving
/// only the v4.1 betas answers 404), or a user the list does not contain.
///
/// IAM v4.0 lists groups but not their members, so every group on the PC is offered to
/// `policy_matches` and a group-granted role counts for every user. The answer errs towards
/// `Yes`, which the action's own 403 then corrects, never towards a `No` that would hide it.
pub async fn resolve(client: &Client, username: &str) -> CanIndex {
    let list = |id: &'static str| async move {
        let kind =
            nutsh_catalog::kind(id).ok_or_else(|| PrismError::Catalog(format!("no kind {id}")))?;
        client.list_all(kind, &ListOptions::default()).await
    };
    // The whole list rather than `$filter=username eq '...'`: the mock ignores `$filter`, so a
    // filter would run untested until it does. The client-side match is the authority either way.
    let users = match list("iam.authn.User").await {
        Ok(u) => u,
        Err(e) => return CanIndex::unknown(format!("cannot read users ({e})")),
    };
    let Some(me) = users.iter().find(|u| {
        u.raw
            .get("username")
            .and_then(Value::as_str)
            .is_some_and(|n| n.eq_ignore_ascii_case(username))
    }) else {
        return CanIndex::unknown(format!("{username} is not in the IAM user list"));
    };
    // Not `unwrap_or_default()`: an unreadable group list would turn a group-granted role into
    // a false `No`, and `No` is a refusal.
    let groups = match list("iam.authn.UserGroup").await {
        Ok(g) => g,
        Err(e) => return CanIndex::unknown(format!("cannot read user groups ({e})")),
    };
    let policies = match list("iam.authz.AuthorizationPolicy").await {
        Ok(p) => p,
        Err(e) => return CanIndex::unknown(format!("cannot read authorization policies ({e})")),
    };
    let roles = match list("iam.authz.Role").await {
        Ok(r) => r,
        Err(e) => return CanIndex::unknown(format!("cannot read roles ({e})")),
    };
    let group_ids: Vec<String> = groups.iter().map(|g| g.ext_id.clone()).collect();
    let group_names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
    // A set, because a system-defined policy and a custom one granting the same role is common
    // and `:can-i` reports each role once.
    let mine: BTreeSet<String> = policies
        .iter()
        .filter(|p| policy_matches(&p.raw, &me.ext_id, username, &group_ids, &group_names))
        .filter_map(|p| p.raw.get("role").and_then(Value::as_str))
        .filter_map(|role| {
            roles
                .iter()
                .find(|r| r.ext_id == role)
                .and_then(|r| r.raw.get("displayName").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect();
    CanIndex::with_roles(mine.into_iter().collect())
}

/// Whether a policy names this identity. Defensive by necessity: `identityFilter` has no
/// declared sub-structure in any IAM spec version, so every string leaf under it is compared,
/// case-insensitively, against the user's extId, the username, and each group's extId and
/// name. An empty leaf names nobody: an entity without an `extId`, or a group without a
/// `name`, carries `""`, which must not pair with a `""` in some policy's filter.
pub fn policy_matches(
    policy: &Value,
    user_ext_id: &str,
    username: &str,
    group_ids: &[String],
    group_names: &[String],
) -> bool {
    let names_me = |leaf: &str| {
        !leaf.is_empty()
            && (leaf.eq_ignore_ascii_case(user_ext_id)
                || leaf.eq_ignore_ascii_case(username)
                || group_ids.iter().any(|g| leaf.eq_ignore_ascii_case(g))
                || group_names.iter().any(|g| leaf.eq_ignore_ascii_case(g)))
    };
    policy
        .get("identities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|identity| identity.get("identityFilter"))
        .any(|filter| any_string(filter, &names_me))
}

/// Whether any string leaf under `v` satisfies `f`, stopping at the first that does.
fn any_string(v: &Value, f: &impl Fn(&str) -> bool) -> bool {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().any(|x| any_string(x, f)),
        Value::Object(o) => o.values().any(|x| any_string(x, f)),
        _ => false,
    }
}
