//! The one rule three callers share: the recorder, which replaces a hit with a placeholder, the
//! generator, which refuses to make a column of it, and the cache, which deletes the key. The
//! lists live here so no two of them can drift, and this file is the keep-list's only home.

use nutsh_catalog::KINDS;
use nutsh_catalog::secret::{
    OPAQUE_KEY, SECRET_KEY_KEEP, SECRET_KEY_MARKERS, is_key_material, is_secret_name,
};

/// Every name on the keep-list survives the markers that catch it. Each one *describes* a
/// secret instead of being one: an extId, a name, a status, an issuer, a policy type, a flag.
#[test]
fn every_kept_name_survives_the_markers() {
    for name in SECRET_KEY_KEEP {
        assert!(
            !is_secret_name(name),
            "{name} is on the keep-list and must not be scrubbed"
        );
        assert_eq!(
            *name,
            name.to_ascii_lowercase(),
            "the keep-list is matched against a lowercased key, so it is spelled lowercase"
        );
        assert!(
            SECRET_KEY_MARKERS.iter().any(|m| name.contains(m)),
            "{name} is on the keep-list for no reason: no marker catches it"
        );
    }
    // Exact: a name that merely starts with a kept one is still a secret.
    assert!(is_secret_name("accessKeyNameSecret"));
}

/// What this rule exists for: `script` as a bare *contains* marker matches `de-script-ion`,
/// which would redact `operationDescription` on every row of the committed tasks fixture. It is anchored as a suffix, which is the shape every secret-bearing one has
/// and no `…Description` does.
#[test]
fn a_description_is_not_a_script() {
    for kept in [
        "description",
        "entitydescription",
        "hostdescription",
        "operationdescription",
        "stepdescription",
        "templatedescription",
        "threatdescription",
        "versiondescription",
    ] {
        assert!(!is_secret_name(kept), "{kept} carries no secret");
    }
    for scrubbed in [
        "cloudinitscript",
        "sysprepscript",
        "userdatascript",
        "customizationscript",
        "firstbootscript",
    ] {
        assert!(is_secret_name(scrubbed), "{scrubbed} carries a payload");
    }
    // And the flags *around* a script are booleans and objects, not payloads.
    assert!(!is_secret_name("shouldenablescriptexec"));
    assert!(!is_secret_name("inguestscriptexecutionconfig"));
}

/// The markers themselves, matched anywhere in the key because each is a whole word that only
/// ever names a credential.
#[test]
fn the_real_secrets_are_caught_wherever_they_sit_in_the_name() {
    for scrubbed in [
        "password",
        "privatekey",
        "privatekeypassphrase",
        "secretaccesskey",
        "clientsecret",
        "targetsecret",
        "reclaimtoken",
        "authorizedpublickeylist",
        "sshkeyvalue",
        "apikey",
        "cookie",
    ] {
        assert!(is_secret_name(scrubbed), "{scrubbed}");
    }
    assert_eq!(OPAQUE_KEY, "guestCustomization");
}

/// A bare `key` is normally one half of a category pair; in the recording it is also twenty real SSH
/// public keys. The value's shape is what tells them apart, because the name cannot.
#[test]
fn key_material_is_recognised_by_the_shape_of_its_value() {
    assert!(is_key_material("ssh-rsa AAAAB3NzaC1yc2E..."));
    assert!(is_key_material("ecdsa-sha2-nistp256 AAAA..."));
    assert!(is_key_material("-----BEGIN OPENSSH PRIVATE KEY-----"));
    assert!(is_key_material(&"x".repeat(101)), "a long opaque blob");
    assert!(!is_key_material("environment"), "half a category pair");
    assert!(!is_key_material("prod"));
}

/// The prerequisite, made executable: no column a table can draw may be blanked by the scrub.
/// The generator refuses secret-shaped columns -
/// `generated.rs` once curated `password`, `privateKey`, `privateKeyPassphrase`,
/// `reclaimToken` and `secretAccessKey`, among others, across thirteen kinds. That refusal is
/// upstream and this assertion is how this task knows it has landed.
///
/// Both lists, because `tui::table::columns` draws `fallback_columns` whenever `columns` is
/// empty - `iam.authn.User` is the kind where that is true today, and its `bucketsAccessKeys`
/// is why the keep-list has a list on it as well as three flags. The generator's own refusal
/// cannot cover those: it is guarded by `ty == "string"`, and the cache deletes a key whatever
/// its value's type.
#[test]
fn no_curated_column_is_blanked_by_the_scrub() {
    for kind in KINDS {
        for c in kind.columns.iter().chain(kind.fallback_columns) {
            let root = c.path.split(['.', '[']).next().unwrap_or(c.path);
            assert!(
                !is_secret_name(root),
                "{}: column {:?} would be blanked in a cached row",
                kind.id,
                c.path
            );
        }
    }
}
