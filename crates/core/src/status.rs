//! Status words to colour roles.
//!
//! The vocabulary is an enum census of `specs/`, not an invention: every word below appears in
//! at least one v4 enum. A word that is not here is `Neutral`, which is the standard row
//! colour, so an unknown status is drawn like an ordinary row rather than like a problem.

use nutsh_catalog::{Kind, Role};

/// Trim, strip a leading `$`, fold `-` and space to `_`, uppercase.
///
/// Re-exported from the catalog rather than written twice: the generator normalises
/// `curated.toml`'s override words with the same function, so a `[kinds.*.status]` key and the
/// word it is meant to match can never drift apart.
pub use nutsh_catalog::normalize_status as normalize;

/// The role of a status word, by the shared vocabulary alone.
pub fn role(word: &str) -> Role {
    role_of(&normalize(word))
}

/// The role of a status word for one kind: its `curated.toml` overrides first, then the
/// vocabulary. Normalised once for both lookups: this runs per row per frame.
pub fn role_in(kind: &Kind, word: &str) -> Role {
    let word = normalize(word);
    kind.status_role(&word).unwrap_or_else(|| role_of(&word))
}

/// The vocabulary, over a word already normalised.
fn role_of(word: &str) -> Role {
    match word {
        "ON" | "RUNNING" | "SUCCEEDED" | "SUCCESS" | "COMPLETE" | "COMPLETED" | "NORMAL" | "UP"
        | "ACTIVE" | "AVAILABLE" | "HEALTHY" | "RESOLVED" | "AUTO_RESOLVED" | "ACKNOWLEDGED"
        | "CONNECTED" | "ENABLED" | "READY" | "IN_SYNC" | "RECOVERY_COMPLETED" | "TRUE" => Role::Ok,
        "OFF" | "POWERED_OFF" => Role::Off,
        "PAUSED"
        | "SUSPENDED"
        | "QUEUED"
        | "PENDING"
        | "IN_PROGRESS"
        | "MIGRATING"
        | "CANCELING"
        | "CANCELLING"
        | "SUSPENDING"
        | "RESUMING"
        | "PAUSING"
        | "POWERING_ON"
        | "POWERING_OFF"
        | "PROVISIONING"
        | "UPDATING"
        | "UPGRADING"
        | "DOWNLOADING"
        | "PREUPGRADE"
        | "SCHEDULED"
        | "RETRYING"
        | "CONFIGURING"
        | "MAINTENANCE"
        | "NEW_NODE"
        | "SETUP_PENDING"
        | "SYNCING"
        | "DELETING_IN_PROGRESS"
        | "FAILOVER_IN_PROGRESS"
        | "RECOVERY_STARTED"
        | "RECOVERY_IN_PROGRESS" => Role::Pending,
        "FAILED" | "ABORTED" | "ERROR" | "CRITICAL" | "HIGH" | "DOWN" | "DEGRADED"
        | "UNHEALTHY" | "UNREACHABLE" | "DISCONNECTED" | "OUT_OF_SYNC" | "DELETE_FAILED"
        | "FAILOVER_FAILED" | "FALSE" => Role::Error,
        "WARNING" | "MEDIUM" | "COMPLETED_WITH_ERRORS" | "SUCCEEDED_WITH_VALIDATION_WARNING" => {
            Role::Warn
        }
        "INFO" | "LOW" => Role::Info,
        "CANCELED" | "CANCELLED" | "DELETING" | "DELETED" | "TERMINATING" | "TO_BE_REMOVED"
        | "OK_TO_BE_REMOVED" => Role::Ending,
        // `_` is what a bare `-` normalises to: the separator fold runs over the whole word.
        "UNKNOWN" | "REDACTED" | "UNDETERMINED" | "NONE" | "DEFAULT" | "DISABLED" | "INACTIVE"
        | "SKIPPED" | "" | "_" => Role::Muted,
        _ => Role::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nutsh_catalog::Role;

    #[test]
    fn the_vocabulary_is_the_census_of_the_specs() {
        for word in [
            "ON",
            "RUNNING",
            "SUCCEEDED",
            "SUCCESS",
            "COMPLETE",
            "COMPLETED",
            "NORMAL",
            "UP",
            "ACTIVE",
            "AVAILABLE",
            "HEALTHY",
            "RESOLVED",
            "AUTO_RESOLVED",
            "ACKNOWLEDGED",
            "CONNECTED",
            "ENABLED",
            "READY",
            "IN_SYNC",
            "RECOVERY_COMPLETED",
            "TRUE",
        ] {
            assert_eq!(role(word), Role::Ok, "{word}");
        }
        for word in ["OFF", "POWERED_OFF"] {
            assert_eq!(role(word), Role::Off, "{word}");
        }
        for word in [
            "PAUSED",
            "SUSPENDED",
            "QUEUED",
            "PENDING",
            "IN_PROGRESS",
            "MIGRATING",
            "CANCELING",
            "CANCELLING",
            "SUSPENDING",
            "RESUMING",
            "PAUSING",
            "POWERING_ON",
            "POWERING_OFF",
            "PROVISIONING",
            "UPDATING",
            "UPGRADING",
            "DOWNLOADING",
            "PREUPGRADE",
            "SCHEDULED",
            "RETRYING",
            "CONFIGURING",
            "MAINTENANCE",
            "NEW_NODE",
            "SETUP_PENDING",
            "SYNCING",
            "DELETING_IN_PROGRESS",
            "FAILOVER_IN_PROGRESS",
            "RECOVERY_STARTED",
            "RECOVERY_IN_PROGRESS",
        ] {
            assert_eq!(role(word), Role::Pending, "{word}");
        }
        for word in [
            "FAILED",
            "ABORTED",
            "ERROR",
            "CRITICAL",
            "HIGH",
            "DOWN",
            "DEGRADED",
            "UNHEALTHY",
            "UNREACHABLE",
            "DISCONNECTED",
            "OUT_OF_SYNC",
            "DELETE_FAILED",
            "FAILOVER_FAILED",
            "FALSE",
        ] {
            assert_eq!(role(word), Role::Error, "{word}");
        }
        for word in [
            "WARNING",
            "MEDIUM",
            "COMPLETED_WITH_ERRORS",
            "SUCCEEDED_WITH_VALIDATION_WARNING",
        ] {
            assert_eq!(role(word), Role::Warn, "{word}");
        }
        for word in ["INFO", "LOW"] {
            assert_eq!(role(word), Role::Info, "{word}");
        }
        for word in [
            "CANCELED",
            "CANCELLED",
            "DELETING",
            "DELETED",
            "TERMINATING",
            "TO_BE_REMOVED",
            "OK_TO_BE_REMOVED",
        ] {
            assert_eq!(role(word), Role::Ending, "{word}");
        }
        for word in [
            "UNKNOWN",
            "REDACTED",
            "UNDETERMINED",
            "NONE",
            "DEFAULT",
            "DISABLED",
            "INACTIVE",
            "SKIPPED",
            "",
            "-",
        ] {
            assert_eq!(role(word), Role::Muted, "{word:?}");
        }
    }

    /// Both spellings of CANCEL(L)ED are in the table because the specs disagree with
    /// themselves: `prism.config.TaskStatus` and `lifecycle.common.OperationStatus` use one
    /// `L`, `clustermgmt.config.UpgradeStatus` and `files.operations.JobStatus` use two.
    #[test]
    fn both_spellings_of_cancelled_are_known() {
        assert_eq!(role("CANCELED"), Role::Ending);
        assert_eq!(role("CANCELLED"), Role::Ending);
        assert_eq!(role("CANCELING"), Role::Pending);
        assert_eq!(role("CANCELLING"), Role::Pending);
    }

    /// The four normalisations, and the `$` in particular: every enum in `specs/` carries
    /// `$UNKNOWN` and all but four also carry `$REDACTED`.
    #[test]
    fn words_are_normalised_before_they_are_matched() {
        assert_eq!(role("  on  "), Role::Ok);
        assert_eq!(role("on"), Role::Ok);
        assert_eq!(role("$UNKNOWN"), Role::Muted);
        assert_eq!(role("$REDACTED"), Role::Muted);
        assert_eq!(role("in-progress"), Role::Pending);
        assert_eq!(role("Powered Off"), Role::Off);
        assert_eq!(
            role("normal"),
            Role::Ok,
            "Host maintenanceState is lowercase"
        );
    }

    #[test]
    fn a_word_the_vocabulary_does_not_know_is_neutral() {
        assert_eq!(role("BANANA"), Role::Neutral);
        assert_eq!(role("kPending"), Role::Neutral, "v3 spellings are not v4");
    }

    /// A curated override wins over the vocabulary, and only for the kind that declares it.
    #[test]
    fn role_in_prefers_the_kinds_override() {
        let task = nutsh_catalog::kind("prism.config.Task").expect("the catalog has Tasks");
        let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
        assert_eq!(role("QUEUED"), Role::Pending);
        assert_eq!(role_in(task, "QUEUED"), Role::Muted);
        assert_eq!(role_in(task, "queued"), Role::Muted, "normalised first");
        assert_eq!(role_in(vm, "QUEUED"), Role::Pending);
        assert_eq!(role_in(task, "RUNNING"), Role::Ok);
    }
}
