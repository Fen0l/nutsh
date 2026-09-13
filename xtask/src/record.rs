//! Record redacted fixtures from a live Prism Central into `crates/mockpc/fixtures-lab`.
//!
//! Recording never targets the curated `crates/mockpc/fixtures` tree: those files are
//! test inputs, and a recording would silently rewrite what the suite asserts against.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow, bail};
use nutsh_catalog::secret::{OPAQUE_KEY, is_key_material, is_secret_name};
use nutsh_catalog::{KINDS, Kind, lookup};
use nutsh_prism::{Client, ListOptions, Profile};
use regex::Regex;
use serde_json::Value;

pub struct RecordArgs {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub insecure: bool,
    pub ca_bundle: Option<PathBuf>,
    pub kinds: Vec<String>,
    pub pages: u32,
    pub out: PathBuf,
    /// The curated `crates/mockpc/fixtures` tree, which `out` must never resolve to.
    pub curated_dir: PathBuf,
}

/// Takes `NUTSH_PASSWORD` out of this process's environment, so that child processes do not
/// inherit it, and returns it. `None` means the caller should prompt instead; a value that
/// is not UTF-8 is an error rather than a silent fall-through to the prompt.
///
/// Must be called from the top of `main`, before the runtime or any other thread exists.
pub fn env_password() -> Result<Option<String>> {
    let Some(raw) = std::env::var_os("NUTSH_PASSWORD") else {
        return Ok(None);
    };
    // SAFETY: called from `main` before the tokio runtime or any other thread is
    // created, so no other thread can be reading the environment concurrently.
    unsafe { std::env::remove_var("NUTSH_PASSWORD") };
    let p = raw
        .into_string()
        .map_err(|_| anyhow!("NUTSH_PASSWORD is not valid UTF-8"))?;
    Ok(Some(p))
}

/// Namespaces held back from a default recording: identity, RBAC, licensing, multi-tenancy.
const EXCLUDED_NAMESPACES: &[&str] = &["iam", "security", "licensing", "tenancy"];
/// Substrings of a kind id that mark it as identity- or secret-bearing.
const EXCLUDED_KIND_MARKERS: &[&str] = &[
    "Credential",
    "Password",
    "Certificate",
    "KeyManagement",
    "LicenseKey",
    "User",
];

fn excluded_by_default(kind: &Kind) -> bool {
    EXCLUDED_NAMESPACES.contains(&kind.namespace)
        || EXCLUDED_KIND_MARKERS.iter().any(|m| kind.id.contains(m))
}

/// An existing path resolved through symlinks and `..`; anything else as written, so two
/// spellings of the same directory compare equal without requiring either to exist.
fn resolved(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

pub async fn record(args: RecordArgs) -> Result<()> {
    if resolved(&args.out) == resolved(&args.curated_dir) {
        bail!(
            "refusing to record into the curated fixture directory {}; use --out",
            args.out.display()
        );
    }
    let profile = Profile {
        host: args.host,
        port: args.port,
        username: args.username,
        verify_tls: !args.insecure,
        ca_bundle: args.ca_bundle,
        plain_http: false,
    };
    let client = Client::connect(&profile, &args.password)?;
    // Fixtures are keyed by the catalog path; the request goes to whatever version the PC
    // serves, so an older PC can still be recorded.
    client.negotiate().await?;
    let kinds: Vec<&'static Kind> = if args.kinds.is_empty() {
        let listable: Vec<&'static Kind> = KINDS
            .iter()
            .filter(|k| k.is_top_level() && !k.preview && !k.list_params.required)
            .collect();
        let kept: Vec<&'static Kind> = listable
            .iter()
            .copied()
            .filter(|k| !excluded_by_default(k))
            .collect();
        eprintln!(
            "excluded {} identity/secret kinds; pass --kinds to record them explicitly",
            listable.len() - kept.len()
        );
        kept
    } else {
        args.kinds
            .iter()
            .map(|t| {
                let kind = lookup(t)
                    .into_iter()
                    .next()
                    .with_context(|| format!("unknown kind {t}"))?;
                if t.as_str() != kind.id {
                    eprintln!("resolved {t} -> {}", kind.id);
                }
                Ok(kind)
            })
            .collect::<Result<_>>()?
    };
    let opts = ListOptions::default();
    let mut refused = 0usize;
    for kind in kinds {
        let mut items = Vec::new();
        for page in 0..args.pages {
            match client.list_page(kind, page, &opts).await {
                Ok(p) => {
                    let n = p.entities.len();
                    items.extend(
                        p.entities
                            .into_iter()
                            .map(|e| redact_from(e.raw, &profile.host)),
                    );
                    // A short page is the last one, and a kind without `$page` has only one.
                    if n < opts.limit as usize || !kind.list_params.page {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("skip {:<40} {e}", kind.id);
                    break;
                }
            }
        }
        if items.is_empty() {
            continue;
        }
        let path = args
            .out
            .join(kind.list_path.trim_start_matches('/'))
            .with_extension("json");
        let count = items.len();
        let items = Value::Array(items);
        let text = serde_json::to_string_pretty(&items)?;
        let found = leaks(&items, &profile.host);
        if !found.is_empty() {
            let head: Vec<&str> = found.iter().take(3).map(String::as_str).collect();
            eprintln!("refusing to write {}: {}", path.display(), head.join(", "));
            refused += 1;
            continue;
        }
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        std::fs::write(&path, &text)?;
        eprintln!("{:<40} {:>4} -> {}", kind.id, count, path.display());
    }
    if refused > 0 {
        bail!("leak scan refused {refused} fixture(s); those files were not written");
    }
    Ok(())
}

/// Re-apply the masking to fixtures that were recorded earlier, in place.
///
/// A masking rule is tightened after the fact - a real serial found in a committed file -
/// and by then the recording may be unreachable, so re-recording is not on the table. The scrub
/// runs the recorder's own [`redact`] over the committed JSON, one entity at a time, exactly
/// as a recording would. Masking is idempotent, so what changes is what the new rule newly
/// catches. Never the curated tree: those files are hand-written test inputs, and hashing
/// them would rewrite what the suite asserts against.
pub fn scrub(paths: &[PathBuf], curated_dir: &Path, check: bool) -> Result<()> {
    let curated = resolved(curated_dir);
    let mut files = Vec::new();
    for p in paths {
        if resolved(p).starts_with(&curated) {
            bail!(
                "refusing to scrub the curated fixture directory {}: it is test input, not a recording",
                p.display()
            );
        }
        json_files(p, &mut files)?;
    }
    files.sort();
    let (mut changed, mut refused) = (0usize, 0usize);
    for file in &files {
        let text = std::fs::read_to_string(file).with_context(|| file.display().to_string())?;
        let value: Value =
            serde_json::from_str(&text).with_context(|| file.display().to_string())?;
        // A fixture holds one page of entities; the recorder masks each on its own, so the
        // substitution pass stays inside the document that carries both halves of a value.
        let masked = match value {
            Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
            other => redact(other),
        };
        let found = leaks(&masked, "");
        if !found.is_empty() {
            let head: Vec<&str> = found.iter().take(3).map(String::as_str).collect();
            eprintln!("refusing to write {}: {}", file.display(), head.join(", "));
            refused += 1;
            continue;
        }
        let out = serde_json::to_string_pretty(&masked)?;
        if out == text {
            continue;
        }
        changed += 1;
        let lines = text
            .lines()
            .zip(out.lines())
            .filter(|(a, b)| a != b)
            .count();
        eprintln!(
            "{} {:>5} line(s) {}",
            if check { "would change" } else { "rewrote    " },
            lines,
            file.display()
        );
        if !check {
            std::fs::write(file, &out).with_context(|| file.display().to_string())?;
        }
    }
    eprintln!(
        "{changed} of {} file(s) {}",
        files.len(),
        if check { "would change" } else { "changed" }
    );
    if refused > 0 {
        bail!("leak scan refused {refused} fixture(s); those files were left as they were");
    }
    Ok(())
}

/// Every `.json` under `p`, or `p` itself when it is one.
fn json_files(p: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if p.is_file() {
        out.push(p.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(p).with_context(|| p.display().to_string())? {
        let path = entry?.path();
        if path.is_dir() {
            json_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

/// What a secret is replaced by, and a fixed point of the masker: whatever key it later
/// turns up under, the placeholder is left alone rather than hashed, so a fixture says
/// plainly that the recorder took the value out.
///
/// The vocabulary beside it - [`OPAQUE_KEY`], the markers, the one suffix, the exceptions and
/// [`is_key_material`] - is [`nutsh_catalog::secret`], shared with the generator and with
/// `nutsh_core::cache` so that what the leak scan takes out of a fixture is what
/// `gen-catalog` refuses to make a column of and what the cache deletes before writing a row.
const REDACTED: &str = "[redacted]";

/// A list under a secret-marked key: keep the shape, redact the leaves. Bare strings in
/// such a list are the secret itself; objects are walked so their own keys are judged on
/// their merits, which is what turns `authorizedPublicKeyList` into a real list of entries
/// whose `key` alone is redacted rather than one opaque `"[redacted]"` string.
fn redact_secret_list(items: Vec<Value>, host: &str, subs: &mut Subs) -> Value {
    Value::Array(
        items
            .into_iter()
            .map(|item| match item {
                Value::String(_) => Value::String(REDACTED.into()),
                Value::Array(inner) => redact_secret_list(inner, host, subs),
                other => redact_value(other, host, subs),
            })
            .collect(),
    )
}

/// Lowercase key names whose string value is hashed, beyond the `*name` suffix and the
/// [`HASHED_KEY_MARKERS`].
///
/// `paramvalue` is the alert parameter bag: `parameters[].paramValue.stringValue` is an
/// untyped carrier whose key promises nothing, and a recording proves what it in fact
/// holds - gateway and load-balancer names, node and block serials, a `PC_<addr>:<cluster>`
/// string. Nothing about the key can tell one from a harmless `/home`, so the whole bag is
/// hashed and the rendered `title` is repaired from it by [`substitute`].
///
/// `vmcategories` is `VmRecoveryPoint.vmCategories`: a list of `key/value` strings whose two
/// halves are both named by whoever made the category, so a recorded one can carry an
/// organisation's name and the name of a Kubernetes cluster - `Owner/Production`,
/// `KubernetesClusterName/mgmt-prod` - into a file bound for the repository. The whole entry
/// is hashed because the key half names the owner as readily as the value half does; every other
/// category field in the corpus is a `categoryExtId`, which is a uuid and stays.
const HASHED_KEYS: &[&str] = &[
    "description",
    "endpoint",
    "fqdn",
    "hostname",
    "username",
    "emailid",
    "createdby",
    "lastupdatedby",
    "ownedby",
    "owner",
    "serviceaccount",
    "firstname",
    "lastname",
    "paramvalue",
    "vmcategories",
];

/// Lowercase substrings that mark a key as identity-bearing wherever they sit in the name,
/// the way [`nutsh_catalog::secret::SECRET_KEY_MARKERS`] does for credentials.
///
/// `serial` is matched by *contains* because the v4 field is `serialId`: an ends-with rule
/// caught `nodeSerial` and `blockSerial` and let every drive serial through, both in
/// `Host.disk[].serialId` and in `Vm.disks[].backingInfo.serialId`. A secret or an
/// identifier is named for what it is, and the name is as often a prefix as a suffix.
const HASHED_KEY_MARKERS: &[&str] = &["serial"];

fn is_hashed_key(lower: &str) -> bool {
    lower.ends_with("name")
        || HASHED_KEY_MARKERS.iter().any(|m| lower.contains(m))
        || HASHED_KEYS.contains(&lower)
}

/// Lowercase key suffixes that carry a hostname the address masker cannot see:
/// `smtpServer`, `endpointUrl`, `searchDomain`, `iscsiTargetAddress`.
///
/// Singular and plural are both spelled out because the API uses both - `searchDomains`,
/// `nameServers`, `learnedIpAddresses` - and a suffix rule that knows only the singular
/// misses the plural exactly as `*serial` misses `serialId`. A recorded `searchDomains` holds
/// real DNS domains.
const HOST_KEY_SUFFIXES: &[&str] = &[
    "url",
    "urls",
    "uri",
    "uris",
    "domain",
    "domains",
    "server",
    "servers",
    "address",
    "addresses",
];

/// A key that carries a hostname. What it carries is judged separately by [`hash_or_mask`]:
/// an address is masked into the documentation ranges rather than hashed, which keeps
/// `ipAddress` and `macAddress` shaped like the real thing.
fn is_host_key(lower: &str) -> bool {
    HOST_KEY_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// Deterministic: identity-bearing strings hashed, addresses masked, secret-looking keys
/// replaced, `$reserved` dropped. extIds are kept so references stay resolvable, and
/// `$`-prefixed keys carry schema identity so their values are left exactly as they are.
///
/// Idempotent: masking an already-masked document returns it unchanged, so [`scrub`] can
/// re-run a tightened rule over a recorded corpus and rewrite only what it newly catches.
pub fn redact(v: Value) -> Value {
    redact_from(v, "")
}

/// [`redact`] that also knows the recorded host, so its name is masked wherever it turns up
/// inside prose (`PC_pc.lab.example is unreachable`), which `leaks()` would otherwise refuse.
///
/// One document at a time: the walk masks by key, and the second pass puts every value the
/// walk hashed out of the rest of the document, where no key names it.
pub fn redact_from(v: Value, host: &str) -> Value {
    let mut subs = Subs::new();
    let v = redact_value(v, host, &mut subs);
    substitute_all(v, subs)
}

fn redact_value(v: Value, host: &str, subs: &mut Subs) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| k != "$reserved")
                .map(|(k, v)| redact_pair(k, v, host, subs))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_value(item, host, subs))
                .collect(),
        ),
        Value::String(s) => Value::String(mask_string(&s, host)),
        other => other,
    }
}

/// One key and what it carries. `$`-prefixed keys carry schema identity, so their values are
/// left exactly as they are.
fn redact_pair(k: String, v: Value, host: &str, subs: &mut Subs) -> (String, Value) {
    if k.starts_with('$') {
        return (k, v);
    }
    let lower = k.to_ascii_lowercase();
    let v = if k == OPAQUE_KEY {
        Value::String(REDACTED.into())
    } else if is_secret_name(&lower) {
        match v {
            Value::Array(items) => redact_secret_list(items, host, subs),
            _ => Value::String(REDACTED.into()),
        }
    } else if k == "key" && matches!(&v, Value::String(s) if is_key_material(s)) {
        Value::String(REDACTED.into())
    } else if lower.ends_with("version") && v.is_string() {
        // A four-part version like `6.5.3.5` is not an address: leave it be.
        v
    } else if is_hashed_key(&lower) || is_host_key(&lower) {
        hash_under(&k, v, host, subs)
    } else {
        redact_value(v, host, subs)
    };
    (k, v)
}

/// What an identity-bearing key carries: the string itself, every string of a list of them
/// (`searchDomains`, `nameServers`), or the payload of a wrapper object.
fn hash_under(key: &str, v: Value, host: &str, subs: &mut Subs) -> Value {
    match v {
        Value::String(s) => hash_or_mask(key, s, host, subs),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| hash_under(key, item, host, subs))
                .collect(),
        ),
        other => redact_wrapped(other, host, subs),
    }
}

/// An address under an identity-bearing key is masked into the documentation ranges rather
/// than hashed, which keeps `ipAddress` and `macAddress` shaped like the real thing.
fn hash_or_mask(key: &str, s: String, host: &str, subs: &mut Subs) -> Value {
    if s.parse::<IpAddr>().is_ok() || is_mac(&s) {
        Value::String(mask_string(&s, host))
    } else {
        Value::String(hash_into(key, &s, subs))
    }
}

/// A hashed key often wraps its string: `"fqdn": { "value": "pc.lab" }`, and the alert bag
/// spells the same shape `"paramValue": { "stringValue": … }`. Hash the inner one, except
/// when it is a uuid: extIds travel through the bag too, and the recording keeps every extId
/// so references stay resolvable. Every other key of the wrapper is judged on its own merits,
/// so a `name` inside one is hashed like any other name.
fn redact_wrapped(v: Value, host: &str, subs: &mut Subs) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| k != "$reserved")
                .map(|(k, v)| match v {
                    Value::String(s)
                        if !k.starts_with('$') && is_wrapped_payload(&k) && !is_uuid(&s) =>
                    {
                        let payload = hash_or_mask(&k, s, host, subs);
                        (k, payload)
                    }
                    other => redact_pair(k, other, host, subs),
                })
                .collect(),
        ),
        other => redact_value(other, host, subs),
    }
}

/// The scalar payload of a wrapper object: `value`, `stringValue`, `ipValue`.
fn is_wrapped_payload(k: &str) -> bool {
    k.to_ascii_lowercase().ends_with("value")
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// FNV-1a 64. Fixed by definition, unlike `DefaultHasher`, so fixture names never churn
/// when the toolchain changes its hashing.
///
/// `nutsh_core::cache::fnv1a64` is the same function and stays a separate copy on purpose:
/// this one feeds names inside committed fixtures, so changing it rewrites the corpus, while
/// changing that one costs a cold start. Do not merge them.
fn hash64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `<key>-<8 hex>`. Idempotent: a value already in that form for this key was hashed by an
/// earlier run and is returned unchanged, which is what makes [`scrub`] a small diff instead
/// of a rehash of the whole corpus.
fn hashed(key: &str, s: &str) -> String {
    if s == REDACTED || is_hashed_as(key, s) {
        return s.to_string();
    }
    format!("{key}-{:08x}", hash64(s) as u32)
}

fn is_hashed_as(key: &str, s: &str) -> bool {
    s.strip_prefix(key)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hex| {
            hex.len() == 8 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// What the walk hashed, in the order it hashed it: the plain value and what replaced it.
type Subs = Vec<(String, String)>;

fn hash_into(key: &str, s: &str, subs: &mut Subs) -> String {
    let out = hashed(key, s);
    if out != s {
        subs.push((s.to_string(), out.clone()));
    }
    out
}

/// A value shorter than this is not substituted: `/home`, `7.6` and `fewer` sit inside
/// timestamps, paths and unrelated prose, where replacing them would corrupt the shape of
/// the fixture without hiding anything that a longer, identity-bearing value does not
/// already hide.
const SUBSTITUTABLE_MIN: usize = 6;

/// Second pass over one document: every value the walk hashed is put out of the strings the
/// walk left alone, because those carry it under a key that names nothing.
///
/// It is the only rule that reaches free text. A drive serial is repeated inside its
/// `mountPath`; an alert renders its `title` from its own parameters, so the gateway name
/// hashed in the bag is still spelled out in the title. Both are the same value in the same
/// document, which is what makes the repair honest rather than a guess at what a name looks
/// like.
fn substitute_all(v: Value, subs: Subs) -> Value {
    let mut pairs: Vec<(String, String)> = subs
        .into_iter()
        .filter(|(from, _)| from.chars().count() >= SUBSTITUTABLE_MIN)
        .collect();
    // Longest first, so a value that contains another is replaced whole, and by name after
    // that, so the result never depends on the order the walk happened to hash in.
    pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.cmp(b)));
    pairs.dedup();
    if pairs.is_empty() {
        return v;
    }
    substitute(v, &pairs)
}

fn substitute(v: Value, pairs: &[(String, String)]) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    // `$`-prefixed keys carry schema identity, as in `redact_value`.
                    if k.starts_with('$') {
                        (k, v)
                    } else {
                        let v = substitute(v, pairs);
                        (k, v)
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| substitute(item, pairs))
                .collect(),
        ),
        Value::String(mut s) => {
            for (from, to) in pairs {
                if s.contains(from.as_str()) {
                    s = s.replace(from.as_str(), to);
                }
            }
            Value::String(s)
        }
        other => other,
    }
}

/// No `\b` anchors: the PC names itself `PC_10.0.0.8`, and `_` is a word character, so a
/// word boundary would let that address through both the masker and the scan.
static IPV4_IN_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\d{1,3}\.){3}\d{1,3}").expect("valid regex"));
static MAC_IN_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:[0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}\b").expect("valid regex"));
static EMAIL_IN_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\w.+-]+@[\w-]+(\.[\w-]+)+").expect("valid regex"));
/// Loose enough to over-match; every hit is confirmed by parsing it as an address.
static IPV6_IN_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{0,4}(?::[0-9a-fA-F]{0,4}){2,7}").expect("valid regex")
});

fn is_mac(s: &str) -> bool {
    s.len() == 17
        && s.split(':').count() == 6
        && s.split(':')
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The ranges the masker maps into, and so the only ones a committed fixture may hold:
/// RFC 5737 documentation IPv4, RFC 3849 documentation IPv6, the IANA documentation MAC and
/// the reserved `.invalid` domain. One definition serves the masker, which leaves an address
/// already inside them alone, and [`scan`], which refuses anything outside them.
const IPV4_DOC: &[[u8; 3]] = &[[192, 0, 2], [198, 51, 100], [203, 0, 113]];
const IPV6_DOC_PREFIX: &str = "2001:db8:";
const MAC_DOC_PREFIX: &str = "00:00:5e:00:53:";
const MASKED_IPV4: &str = "203.0.113.1";
const MASKED_MAC: &str = "00:00:5e:00:53:01";
const MASKED_EMAIL: &str = "user@example.invalid";

fn is_doc_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    IPV4_DOC.contains(&[o[0], o[1], o[2]])
}

/// Already masked once, by this masker or by an earlier recording.
fn is_doc_address(s: &str) -> bool {
    match s.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => is_doc_ipv4(&v4),
        Ok(IpAddr::V6(_)) => s.to_ascii_lowercase().starts_with(IPV6_DOC_PREFIX),
        Err(_) => is_mac(s) && s.to_ascii_lowercase().starts_with(MAC_DOC_PREFIX),
    }
}

fn mask_string(s: &str, host: &str) -> String {
    if is_doc_address(s) {
        return s.to_string();
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        let h = hash64(s);
        return match ip {
            IpAddr::V4(_) if h.is_multiple_of(2) => format!("203.0.113.{}", h % 254 + 1),
            IpAddr::V4(_) => format!("198.51.100.{}", h % 254 + 1),
            IpAddr::V6(_) => format!("2001:db8::{:x}", h % 65535 + 1),
        };
    }
    if is_mac(s) {
        return MASKED_MAC.into();
    }
    // Addresses also hide inside longer prose: alert messages, task error text. A masked one
    // is left as it stands, so a second pass over a recorded fixture is not a reshuffle.
    let s = IPV4_IN_TEXT.replace_all(s, |c: &regex::Captures<'_>| {
        match c[0].parse::<Ipv4Addr>() {
            Ok(ip) if is_doc_ipv4(&ip) => c[0].to_string(),
            _ => MASKED_IPV4.to_string(),
        }
    });
    let s = MAC_IN_TEXT.replace_all(&s, |c: &regex::Captures<'_>| {
        if c[0].to_ascii_lowercase().starts_with(MAC_DOC_PREFIX) {
            c[0].to_string()
        } else {
            MASKED_MAC.to_string()
        }
    });
    let s = EMAIL_IN_TEXT.replace_all(&s, MASKED_EMAIL);
    if host.is_empty() {
        s.into_owned()
    } else {
        s.replace(host, &hashed("host", host))
    }
}

/// Fail-closed scan of a fixture. Every finding is something that must not reach a
/// committed file: a real address, a real e-mail, or the recorded host's name. Structural
/// rather than textual so a `*version` field like `6.5.3.5` is not mistaken for an address.
pub fn leaks(value: &Value, host: &str) -> Vec<String> {
    let mut found = Vec::new();
    walk(value, "", "", host, &mut found);
    found
}

fn walk(value: &Value, path: &str, key: &str, host: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk(v, &child, k, host, found);
            }
        }
        // An array inherits its parent's key: `versionList` holds versions, not addresses.
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                walk(v, &format!("{path}[{i}]"), key, host, found);
            }
        }
        Value::String(s) => {
            // `*version` holds four-part build numbers; `$`-prefixed keys hold schema identity.
            if key.starts_with('$') || key.to_ascii_lowercase().ends_with("version") {
                return;
            }
            scan(s, path, host, found);
        }
        _ => {}
    }
}

/// A match welded to the hex around it is part of a longer identifier, not an address.
///
/// `Disk.serviceVMId` is `<cluster uuid>::<node id>` -
/// `000623f4-7174-ae2a-0000-00000001c1da::9` - whose tail `c1da::9` parses as an IPv6
/// address, and [`scan`] refused every disk fixture the recording recorded. An address is written
/// on its own or set off by punctuation; it is never continued by a hex digit or a UUID's
/// dash, so an adjacent one says the match is a fragment.
fn is_hex_fragment(s: &str, start: usize, end: usize) -> bool {
    let before = s[..start].chars().next_back();
    let after = s[end..].chars().next();
    [before, after]
        .into_iter()
        .flatten()
        .any(|c| c.is_ascii_hexdigit() || c == '-')
}

fn scan(s: &str, path: &str, host: &str, found: &mut Vec<String>) {
    for m in IPV4_IN_TEXT.find_iter(s) {
        if let Ok(ip) = m.as_str().parse::<Ipv4Addr>()
            && !is_doc_ipv4(&ip)
        {
            found.push(format!("{path}: ipv4 {ip}"));
        }
    }
    for m in IPV6_IN_TEXT.find_iter(s) {
        if m.as_str().parse::<Ipv6Addr>().is_ok()
            && !m.as_str().starts_with(IPV6_DOC_PREFIX)
            && !is_hex_fragment(s, m.start(), m.end())
        {
            found.push(format!("{path}: ipv6 {}", m.as_str()));
        }
    }
    for m in MAC_IN_TEXT.find_iter(s) {
        if !m.as_str().to_ascii_lowercase().starts_with(MAC_DOC_PREFIX) {
            found.push(format!("{path}: mac {}", m.as_str()));
        }
    }
    for m in EMAIL_IN_TEXT.find_iter(s) {
        if !m.as_str().ends_with("@example.invalid") {
            found.push(format!("{path}: email {}", m.as_str()));
        }
    }
    if !host.is_empty() && s.contains(host) {
        found.push(format!("{path}: host {host}"));
    }
}
