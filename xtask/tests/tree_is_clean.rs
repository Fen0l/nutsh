//! Every tracked file, read as text and scanned for the network this program was developed
//! against.
//!
//! `fixtures_are_clean.rs` already does this for the fixture trees, structurally, because a
//! fixture is JSON and a recording is the obvious place for a real address to end up. It cannot
//! help anywhere else. What actually got into this tree got in as prose: a task list, a design
//! note, a doc comment, a hand-written test row pasted out of a terminal that was pointed at a
//! real Prism Central. There is no structure in those to walk, so this is a second scanner, over
//! text, and between them they cover everything `git` tracks.
//!
//! **The names are not written in this file.** Spelling them out would publish, in one tidy
//! list, the strings the tree was cleaned of, which is the opposite of the point. They are held
//! as 64-bit FNV-1a hashes of the token they name. That is not secrecy and is not offered as
//! any: a short word behind a fast hash falls to a dictionary in seconds. It is the difference
//! between a repository anybody can grep and one somebody has to attack on purpose, and it means
//! taking a name out of the tree does not quietly put it back in.
//!
//! Two kinds of rule, because two kinds of thing leak. A **token** is a name: a company, a site,
//! a cluster, a person, or the leading octets of an address range. It is matched exactly against
//! the hashes below, so the scan says nothing about anything it does not already know. A
//! **shape** is a host name that can only exist inside somebody's private network, matched by
//! its suffix, which is what catches the next laboratory rather than the last one.

use std::path::{Path, PathBuf};
use std::process::Command;

/// FNV-1a, 64-bit. Six lines and no dependency, over tokens a dozen bytes long.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The tokens no tracked file may contain, hashed. Each is one of: the company whose laboratory
/// this was built against, a name that laboratory gave itself or one of its machines, the
/// author's own name, or the first two octets of its address range.
///
/// To add one, hash the lowercased token with [`fnv1a`] and paste the number. To find out what
/// tripped the scan, read the failure: it prints the file, the line and the token it found,
/// which is text already in front of whoever wrote it.
const FORBIDDEN: &[u64] = &[
    0x283a_b2c5_96d0_f40e,
    0xee5d_acad_45ad_48ce,
    0xf0c7_41ff_dad2_5b05,
    0x2257_28b2_7e07_4a1a,
    0x979e_eefc_7363_6064,
    0x38f5_8ba3_5ea2_bcdb,
    0x9cc2_79e2_a9ba_ced1,
    0x047c_5b9a_22f4_5728,
    0xbf26_ee0d_c52a_caee,
    0xb86a_ac2b_6787_4a3e,
    0xe840_a3d9_724c_f66f,
    0x2c48_9276_28ba_9d7d,
];

/// Suffixes a host name only carries inside somebody's own network. A name ending in one of
/// these was resolved by a private resolver and names a private machine.
const INTERNAL_SUFFIXES: &[&str] = &["local", "lan", "localdomain", "intranet"];

/// Everything `git ls-files` names is scanned except this: the vendored OpenAPI documents.
/// `specs/sync.py` downloads them from the vendor and nobody here edits them, so nothing of this
/// network can reach them, and they carry the vendor's own example addresses and host names by
/// the hundred.
const NOT_OURS: &[&str] = &["specs/"];

/// A dotted part that could be one octet of an address.
fn is_octet(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 3
        && part.bytes().all(|b| b.is_ascii_digit())
        && part.parse::<u16>().is_ok_and(|n| n <= 255)
}

/// The runs of a line that could hold a name: the characters a host name, a machine name or an
/// address is made of, and nothing else.
fn runs(line: &str) -> impl Iterator<Item = String> {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .filter(|run| !run.is_empty())
        .map(str::to_ascii_lowercase)
}

/// Every token one run offers: each part between the dots and hyphens, each also with its
/// trailing digits taken off, because a laboratory numbers its machines and `LAB-THING03` and
/// `LAB-THING07` are the same name twice. A run of four or more octets is an address, and what
/// identifies an address is the network it sits in, so the leading two and three octets go in as
/// tokens of their own.
fn tokens(run: &str) -> Vec<String> {
    let mut out = Vec::new();
    for segment in run.split('-') {
        let parts: Vec<&str> = segment.split('.').collect();
        for part in parts.iter().filter(|p| !p.is_empty()) {
            out.push((*part).to_string());
            let stem: String = part.chars().take_while(char::is_ascii_alphabetic).collect();
            if !stem.is_empty() && stem.len() != part.len() {
                out.push(stem);
            }
        }
        if parts.len() >= 4 && parts.iter().all(|p| is_octet(p)) {
            out.push(format!("{}.{}", parts[0], parts[1]));
            out.push(format!("{}.{}.{}", parts[0], parts[1], parts[2]));
        }
    }
    out
}

/// Whether a run reads as the name of a machine on somebody's private network.
///
/// Two labels with a plain word in front of the suffix is far more likely to be `self.local` in
/// some Rust than a host name. A third label, a digit or a hyphen is what makes it a machine
/// somebody named.
fn is_private_host(run: &str) -> bool {
    let labels: Vec<&str> = run.split('.').collect();
    let Some((suffix, before)) = labels.split_last() else {
        return false;
    };
    if !INTERNAL_SUFFIXES.contains(suffix) {
        return false;
    }
    match before.last() {
        Some(host) if !host.is_empty() => {
            before.len() > 1 || host.bytes().any(|b| b.is_ascii_digit() || b == b'-')
        }
        _ => false,
    }
}

/// What one line gives away, in the reader's own words.
fn findings(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    for run in runs(line) {
        if is_private_host(&run) {
            found.push(format!("the private host name `{run}`"));
        }
        for token in tokens(&run) {
            if FORBIDDEN.contains(&fnv1a(&token)) {
                found.push(format!("the name `{token}`"));
            }
        }
    }
    found
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the workspace")
        .to_path_buf()
}

/// What `git` tracks, which is exactly what publishing this repository would publish. Anything
/// ignored, or merely present on disk, is out of scope by definition: it is not going anywhere.
fn tracked_files(root: &Path) -> Option<Vec<PathBuf>> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .filter(|name| !NOT_OURS.iter().any(|prefix| name.starts_with(prefix)))
            .map(|name| root.join(name))
            .collect(),
    )
}

#[test]
fn no_tracked_file_names_the_network_this_was_built_against() {
    let root = workspace_root();
    let Some(files) = tracked_files(&root) else {
        // A source tree unpacked from an archive has no index to ask. There is then no set of
        // tracked files for this to be about, so it says so rather than passing quietly as
        // though it had looked at something.
        eprintln!(
            "no git index under {}: nothing tracked to scan",
            root.display()
        );
        return;
    };
    assert!(
        !files.is_empty(),
        "git tracks nothing under {}",
        root.display()
    );

    let mut report = Vec::new();
    let mut scanned = 0usize;
    for file in &files {
        // Not text, or gone from disk between the listing and the read. A binary holds no prose
        // to leak, and the one binary-ish thing here is JSON, which the fixture scan covers.
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        scanned += 1;
        let shown = file.strip_prefix(&root).unwrap_or(file.as_path()).display();
        for (n, line) in text.lines().enumerate() {
            for finding in findings(line) {
                report.push(format!("{shown}:{}: {finding}", n + 1));
            }
        }
    }
    assert!(scanned > 0, "no tracked file could be read as text");
    assert!(
        report.is_empty(),
        "{} line(s) name the network this was built against. Replace each with a \
         documentation-safe value: an RFC 5737 address (192.0.2.x, 198.51.100.x, 203.0.113.x), \
         an `example.com` or `.invalid` host name, a neutral cluster name.\n{}",
        report.len(),
        report.join("\n")
    );
}

/// The scan reads a line the way the comment above says it does.
///
/// Every string here is deliberately one the scan must **not** flag, because a test that spelled
/// a forbidden token would put that token back in the tree and the scan would then flag its own
/// source. The tokeniser is checked instead against a documentation address and an invented
/// machine name, which travel the same path a real one would.
#[test]
fn the_scan_reads_a_line_the_way_it_claims_to() {
    assert!(
        findings("let ip = \"192.0.2.8\";").is_empty(),
        "a documentation address is the whole point"
    );
    assert!(
        findings("let n = self.local;").is_empty(),
        "a struct field is not a host name"
    );
    assert!(
        findings("state lives under ~/.local/state/nutsh").is_empty(),
        "a path is not a host name"
    );
    // Assembled from the constant the rule reads rather than written out, because a host name
    // spelled here is a host name in a tracked file, and the scan would rightly flag its own
    // source for it.
    let host = |labels: &str, suffix: &str| format!("host = \"{labels}.{suffix}\"");
    assert_eq!(
        findings(&host("pc-01", INTERNAL_SUFFIXES[0])).len(),
        1,
        "a numbered machine on a private resolver"
    );
    assert_eq!(
        findings(&host("a.b", INTERNAL_SUFFIXES[2])).len(),
        1,
        "three labels and a private suffix"
    );

    let address = tokens("198.51.100.8");
    assert!(
        address.contains(&"198.51".to_string()) && address.contains(&"198.51.100".to_string()),
        "an address is known by the network it sits in: {address:?}"
    );
    let machine = tokens("lab-thing03.example.com");
    assert!(
        machine.contains(&"thing".to_string()),
        "a machine name with its number taken off: {machine:?}"
    );
}
