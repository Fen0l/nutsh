//! Which property names carry a credential.
//!
//! One rule, three callers, and that is the point: `cargo xtask record` takes these values out
//! of a recorded fixture, `cargo xtask gen-catalog` refuses to make a column of them, and
//! `nutsh_core::cache` deletes them out of every row it writes to disk. Each caller keeps its
//! own walk, because they do different things with a hit; only the vocabulary is shared. Two
//! copies of a security rule drift, and the half that drifts is the half nobody is looking at:
//! a name the recorder redacts must never be a name the catalog draws as an ordinary column.
//!
//! It lives in the catalog, beside the rest of the vocabulary, because it is data rather than
//! behaviour: a crate that stores served rows on disk needs the same list and cannot depend on
//! `xtask`.

/// A key is secret when its lowercase form contains one of these. Deliberately not bare
/// `key` and not `metadata`: category and metadata key/value pairs are data, not
/// credentials, and `prism.config.Category` is keyed on `key`. The recorder handles a bare
/// `key` by the shape of its value instead.
///
/// A marker is a whole word that only ever names a credential, so that matching it anywhere
/// in the key is safe. `script` is not one of those and lives in [`SECRET_KEY_SUFFIXES`];
/// the handful of names these markers still catch by accident are in [`SECRET_KEY_KEEP`].
pub const SECRET_KEY_MARKERS: &[&str] = &[
    "password",
    "secret",
    "token",
    "privatekey",
    "apikey",
    "sshkey",
    "publickey",
    "credential",
    "passphrase",
    "accessinformation",
    "cookie",
    "authorization",
    "certificate",
    "licensekey",
    "accesskey",
    "encryptionkey",
    // Guest customization payloads: base64 user data holding passwords, SSH keys, hostnames.
    "cloudinit",
    "sysprep",
    "unattend",
    "userdata",
];

/// The one positional secret rule, and it earns the exception: a script that carries a
/// payload is named for what runs it - `cloudInitScript`, `sysprepScript`, `userDataScript`,
/// `customizationScript` - while the bare word sits inside `description` and
/// `subscriptionId`, which are not secrets at all. As a *contains* marker `script` redacted
/// every description in the specs (82,839 of them) and every `*Description` column the
/// catalog curates, which is how the committed tasks fixture ended up with
/// `"operationDescription": "[redacted]"` on all 100 rows.
///
/// It also spares the flags around a script - `shouldEnableScriptExec`,
/// `inGuestScriptExecutionConfig` - which are booleans and objects, not payloads.
pub const SECRET_KEY_SUFFIXES: &[&str] = &["script"];

/// Exact key names that a marker matches by accident. Each one *describes* a secret instead
/// of being it: an extId, a name, a status, an issuer, a policy type, a flag. Exact by
/// design - a new name that trips a marker is reviewed and added here rather than being
/// waved through by a pattern.
///
/// The three flags are here for the cache rather than for the recorder: `hasPrivateKey`,
/// `shouldValidateAdCredential` and `isForceResetPasswordEnabled` are booleans, so a caller
/// that only judges string values never asked about them, but a caller that deletes a key
/// whatever it holds would blank a column that describes a secret instead of carrying one.
/// `bucketsAccessKeys` is here for the same reason and is the list case rather than the flag
/// case: it is a `Count` column over an array whose members carry `accessKeyName` (kept) and
/// `secretAccessKey` (scrubbed), and a recursive walk judges those on their own merits. Only
/// deleting the list by its own name would blank `iam.authn.User`'s twentieth fallback column
/// while leaving nothing safer on disk.
pub const SECRET_KEY_KEEP: &[&str] = &[
    "hasprivatekey",
    "shouldvalidateadcredential",
    "isforceresetpasswordenabled",
    "bucketsaccesskeys",
    "claimtokenextid",
    "accesskeyname",
    "apicredentialstatus",
    "credentialissuer",
    "authorizationpolicytype",
];

/// Redacted or dropped whole, by exact name. Its payload is base64, so a per-field walk would
/// let the encoded secrets - passwords, SSH keys, hostnames - straight through, and a leak
/// scan cannot see inside it either.
pub const OPAQUE_KEY: &str = "guestCustomization";

/// Whether `name` names a credential. Case-insensitive; the caller passes a bare property
/// name, which is what all three callers have.
///
/// It judges the name alone, and the three callers do not ask it the same question of the same
/// things: the recorder and the generator ask it of `string` values only - the generator's
/// refusal is guarded by `ty == "string"` - while `nutsh_core::cache` asks it of every key
/// whatever the value holds. So the flags and lists *around* a secret - `hasPrivateKey`,
/// `isForceResetPasswordEnabled`, `shouldValidateAdCredential`, `bucketsAccessKeys` - are not
/// spared by their type. They are kept by name, in [`SECRET_KEY_KEEP`], for the cache's sake.
pub fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !SECRET_KEY_KEEP.contains(&lower.as_str())
        && (SECRET_KEY_MARKERS.iter().any(|m| lower.contains(m))
            || SECRET_KEY_SUFFIXES.iter().any(|s| lower.ends_with(s)))
}

/// A bare `key` is normally one half of a category pair, but
/// `Cluster.config.authorizedPublicKeyList[].key` holds real key material. Tell the two apart
/// by the shape of the value, because the name cannot: [`SECRET_KEY_MARKERS`] deliberately
/// omits bare `key` so that `prism.config.Category`, which is keyed on it, stays readable.
pub fn is_key_material(s: &str) -> bool {
    s.starts_with("ssh-")
        || s.starts_with("ecdsa-")
        || s.starts_with("sk-")
        || s.starts_with("-----BEGIN")
        || s.len() > 100
}

#[cfg(test)]
mod tests {
    use super::is_secret_name;

    #[test]
    fn a_marker_matches_anywhere_and_script_only_at_the_end() {
        assert!(is_secret_name("secretAccessKey"), "not a suffix");
        assert!(is_secret_name("PrivateKeyPassphrase"), "case-insensitive");
        assert!(is_secret_name("cloudInitScript"));
        assert!(!is_secret_name("description"), "contains `script`");
        assert!(
            !is_secret_name("scriptedThing"),
            "`script` is a suffix rule"
        );
    }
}
