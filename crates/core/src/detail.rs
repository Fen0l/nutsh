//! What a detail view is: a list of titled sections of labelled values, composed from the
//! catalog's curation where a kind has one and from the entity itself where it does not.
//!
//! Pure data. `crates/tui/src/detail.rs` lays it out and `crates/tui/src/ui.rs` draws it, and
//! nothing here knows that a terminal exists - which is what lets every rule below be a unit
//! test rather than a snapshot.

use std::time::SystemTime;

use nutsh_catalog::{
    Column, ColumnKind, DetailSection, Kind, OBJECT_TYPE, When, object_type_prefix, parse_when,
    short_type, words,
};
use nutsh_prism::Entity;
use serde_json::Value;

use crate::cell::{self, Names, Rendered};
use crate::path;

/// One titled group of labelled values, ready to lay out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub fields: Vec<(String, Rendered)>,
}

/// The sections of `entity`: curated where `kind.detail` is non-empty, composed from the entity
/// itself where it is not. Empty sections are already dropped, so a caller draws what it is
/// given and decides nothing.
pub fn compose(kind: &Kind, entity: &Entity, names: &Names, now: SystemTime) -> Vec<Section> {
    if kind.detail.is_empty() {
        return fallback(kind, entity, names, now);
    }
    curated(kind.detail, entity, names, now)
}

/// The curated half, over any list of sections: `compose` passes `kind.detail` and the tests
/// pass their own, so a rule can be asserted without curating a kind to assert it.
pub fn curated(
    sections: &[DetailSection],
    entity: &Entity,
    names: &Names,
    now: SystemTime,
) -> Vec<Section> {
    let mut out = Vec::new();
    for s in sections {
        if !s.when.is_none_or(|w| holds(&entity.raw, w)) {
            continue;
        }
        let fields: Vec<(String, Rendered)> = s
            .fields
            .iter()
            .filter(|f| f.when.is_none_or(|w| holds(&entity.raw, w)))
            .map(|f| {
                (
                    f.label.to_string(),
                    cell::render_value(f.path, f.kind, entity, names, now),
                )
            })
            .collect();
        push_if_any(&mut out, s.title.to_string(), fields);
    }
    out
}

/// The empty-section rule: a section is kept iff at least one of its fields rendered to
/// something other than the dim `-`. A powered-off VM with no guest tools drops "Guest"; a VM
/// with no `bootConfig` drops both boot sections. `✗` and `0` are values, so a section of
/// all-false flags is kept - which is what makes it safe to curate generously.
fn push_if_any(out: &mut Vec<Section>, title: String, fields: Vec<(String, Rendered)>) {
    if fields.iter().any(|(_, r)| !is_blank(r)) {
        out.push(Section { title, fields });
    }
}

/// The one cell `cell::render_value` returns for a missing value, a `null`, an empty string, or
/// a fan-out whose every value was one of those.
fn is_blank(r: &Rendered) -> bool {
    r.dim && r.text == "-"
}

/// The `when` grammar over one entity. Anything outside the grammar is `false`: the generator
/// refuses it, so this arm is unreachable through the catalog and safe for a hand-built `Kind`.
pub fn holds(raw: &Value, when: &str) -> bool {
    match parse_when(when) {
        None => false,
        Some(When::Present(p)) => present(raw, p),
        Some(When::Absent(p)) => !present(raw, p),
        Some(When::Equals { path: p, value }) => {
            let Some(found) = path::first(raw, p) else {
                return false;
            };
            let Some(text) = found.as_str() else {
                // A number or a flag compared as it prints: `numSockets = 2`, `isAgentVm = true`.
                return found.to_string().eq_ignore_ascii_case(value);
            };
            // A `$objectType` is compared on its short name: the same shape is
            // `vmm.v4.ahv.config.UefiBoot` on one Prism Central build and
            // `vmm.v4.r0.b1.ahv.config.UefiBoot` on the next, and the short name is the only
            // half that is stable.
            let text = if object_type_prefix(p).is_some() {
                short_type(text)
            } else {
                text
            };
            text.eq_ignore_ascii_case(value)
        }
    }
}

/// A path that resolves to a value that is not `null`.
fn present(raw: &Value, p: &str) -> bool {
    matches!(path::first(raw, p), Some(v) if !v.is_null())
}

/// The detail of a kind nobody curated: its table row stood up, and then everything else the
/// Prism Central returned, grouped.
///
/// Computed here rather than generated, for two reasons. 262 kinds × 6 sections × 8 fields is
/// roughly 12 500 field literals appended to a `generated.rs` that is already 927 KB; and a
/// generated list would show a phantom row for every optional property the Prism Central chose
/// not to return, where this shows what actually came back.
pub fn fallback(kind: &Kind, entity: &Entity, names: &Names, now: SystemTime) -> Vec<Section> {
    let mut out = Vec::new();
    summary(kind, entity, names, now, &mut out);
    let shown = shown_by(kind);
    let Some(top) = entity.raw.as_object() else {
        return out;
    };
    let raw = &entity.raw;
    // A top-level key the Summary showed *whole* is not shown again. One it merely reached into
    // - `userReference.name`, `config.clusterFunction` - keeps its section; the single leaf the
    // column took is dropped from it, and its siblings stay.
    let mine =
        |key: &String| !is_noise(key) && !shown.iter().any(|p| p.trim_end_matches("[]") == key);
    let already = |key: &str, leaf: &str| {
        let path = format!("{key}.{leaf}");
        shown.iter().any(|p| *p == path)
    };

    // One pass down the document, because "inlined or a section of its own" is one decision made
    // once. Rule 2 fills `Details`: every remaining top-level scalar in wire order, every
    // top-level array of scalars through the fan-out rule, and every top-level object holding
    // `INLINE_MAX` leaves or fewer, as dotted fields (`Cluster · extId`). Rules 3 and 4 fill
    // `sections`, in the document's own order so the frame reads down the entity: one section per
    // top-level object with more than `INLINE_MAX` leaves, titled by its key and holding those
    // leaves labelled by leaf segment (the nesting is in the title), and one section per
    // top-level array of objects, titled `"{Key} ({n})"`.
    let mut details = Vec::new();
    let mut sections: Vec<(String, Vec<(String, Rendered)>)> = Vec::new();
    for (key, value) in top.iter().filter(|(k, _)| mine(k)) {
        match value {
            Value::Object(_) => {
                let mut sub = Vec::new();
                leaves(value, "", DEEP, &mut sub);
                // The document decides which of the two an object is, and curation decides only
                // what is repeated: a column that took one leaf does not turn a six-leaf object
                // into an inlined pair.
                let own_section = sub.len() > INLINE_MAX;
                sub.retain(|leaf| !already(key, &leaf.path));
                if own_section {
                    let paths: Vec<&str> = sub.iter().map(|l| l.path.as_str()).collect();
                    let fields = label_leaves(&paths)
                        .into_iter()
                        .zip(&sub)
                        .map(|(label, leaf)| {
                            field(
                                label,
                                &format!("{key}.{}", leaf.path),
                                &leaf.sample,
                                raw,
                                names,
                                now,
                            )
                        })
                        .collect();
                    sections.push((label_of(key), fields));
                } else {
                    details.extend(sub.into_iter().map(|leaf| {
                        field(
                            inline_label(key, &leaf.path),
                            &format!("{key}.{}", leaf.path),
                            &leaf.sample,
                            raw,
                            names,
                            now,
                        )
                    }));
                }
            }
            Value::Null => {}
            Value::Array(items) if items.iter().all(|i| !i.is_object() && !i.is_array()) => {
                if let Some(sample) = items.first() {
                    details.push(field(
                        label_of(key),
                        &format!("{key}[]"),
                        sample,
                        raw,
                        names,
                        now,
                    ));
                }
            }
            Value::Array(items) if items.iter().any(Value::is_object) => {
                sections.push((
                    format!("{} ({})", label_of(key), items.len()),
                    elements(items, names, now),
                ));
            }
            Value::Array(_) => {}
            scalar => details.push(field(label_of(key), key, scalar, raw, names, now)),
        }
    }
    push_if_any(&mut out, "Details".to_string(), details);
    for (title, fields) in sections {
        push_if_any(&mut out, title, fields);
    }
    out
}

/// Rule 1: the kind's columns turned on their side - the header sentence-cased is the label, and
/// the path and kind are the column's - with `UUID` at `kind.ext_id_key` prepended, because the
/// table never shows it and the detail is where a reader goes for it.
///
/// The **whole** column list, not the six a table draws: the detail has no `w`, so the columns
/// past the sixth have nowhere else to be read, and a derived list is capped at twenty by the
/// generator, inside the twenty-four a curated section may hold.
fn summary(kind: &Kind, entity: &Entity, names: &Names, now: SystemTime, out: &mut Vec<Section>) {
    let mut fields = vec![(
        "UUID".to_string(),
        // `Text`, not `Reference`: this is the identifier, not a reference to one, and an
        // eight-character stub of the row you are looking at helps nobody.
        cell::render_value(kind.ext_id_key, ColumnKind::Text, entity, names, now),
    )];
    fields.extend(columns_of(kind).iter().map(|c| {
        (
            header_label(c.header),
            cell::render_value(c.path, c.kind, entity, names, now),
        )
    }));
    push_if_any(out, "Summary".to_string(), fields);
}

/// The column list rule 1 stands up: the curated one where a kind has one, and the list the
/// generator derived where it does not.
fn columns_of(kind: &Kind) -> &'static [Column] {
    if kind.columns.is_empty() {
        kind.fallback_columns
    } else {
        kind.columns
    }
}

/// A column header as a field label: `POWER STATE` → `Power state`, `EXT ID` → `Ext ID`.
///
/// `words::sentence_case` splits on `_`, so a header's spaces are folded to underscores first;
/// the catalog's acronym table then spells `ID`, `IP` and `VM` the way a person writes them.
fn header_label(header: &str) -> String {
    words::sentence_case(&header.replace(' ', "_"))
}

/// How deep a rule-3 section follows its object. Far enough that nothing in the recorded corpus
/// is cut - the deepest leaf is `guestTools.guestInfo.dnsName.value`, three levels down - and
/// shallow enough that a pathological document cannot make one section a page long. Anything
/// past it is simply not shown; the raw view (`Y`) is where a document is read whole.
const DEEP: usize = 6;
/// A top-level object with this many leaves or fewer is inlined into `Details` rather than given
/// a section. Measured in the recording VM: of seven top-level objects, five hold exactly one leaf
/// (`cluster.extId`, `project.extId`, `vtpmConfig.isVtpmEnabled`, `apcConfig.isApcEnabled`,
/// `storageConfig.isFlashModeEnabled`). A section apiece would spend fourteen lines of a
/// twenty-line pane on seven facts, each under its own heading; inlining puts those seven on
/// four lines and leaves the two real sections standing.
const INLINE_MAX: usize = 2;
/// Elements of an array of objects shown, one line each. The rest live behind `enter`.
const ELEMENTS: usize = 3;
/// Scalars per element line.
const ELEMENT_SCALARS: usize = 4;
/// How deep an element line reaches for its scalars, as a [`leaves`] budget: `1` follows one
/// more level of nesting, which is the design's "depth of 2" - the element's own keys and the
/// keys one below them.
///
/// Two levels rather than one is not a detail: a VM disk's only *top-level* scalars are `extId`
/// and `$objectType`, so a one-level rule would print a column of ext ids while
/// `diskAddress.busType`, `diskAddress.index` and `backingInfo.diskSizeBytes` - the things a
/// person opened the section for - sat one below.
const ELEMENT_DEPTH: usize = 1;

/// Noise, dropped at every level of every walk, matched on a key's last dotted segment.
///
/// Measured honestly. `$reserved` and `tenantId` appear in **zero** fixtures - they are in the
/// `ExternalizableAbstractModel` base every v4 schema inherits, so the entries are cheap
/// insurance rather than observed volume - and `links` appears only on storage containers and
/// host NICs, as self-hrefs. The volume is `$objectType`: 3944 occurrences in the recording `vms.json`
/// alone. A nested `extId` is deliberately **not** here - 1687 occurrences, every one of which
/// renders as a `Reference` and resolves to a name wherever one is known, which is the opposite
/// of noise.
///
/// `$objectType` is not pure noise either, and this respects that: it is never a *field*, but
/// its short name is the **variant tag** of the object it sits on, which is what labels a rule-4
/// element line and what a curated `when` compares against.
const NOISE: &[&str] = &[OBJECT_TYPE, "$reserved", "links", "tenantId"];

fn is_noise(key: &str) -> bool {
    NOISE.contains(&key.rsplit('.').next().unwrap_or(key))
}

/// One value the fallback will draw: the dotted path to read it back with, and a sample of the
/// value for [`infer`].
struct Leaf {
    path: String,
    sample: Value,
}

/// The scalar leaves under `value`, depth-first in wire order. `budget` is how many further
/// levels of object nesting are followed; `0` takes this level's scalars only. An array of
/// scalars is one leaf, taken through the fan-out path `key[]` and sampled from its first
/// element; an array of objects is not a leaf at all. A `null` is not a leaf either: the
/// fallback shows what the Prism Central returned, so an omitted property leaves no phantom row.
fn leaves(value: &Value, prefix: &str, budget: usize, out: &mut Vec<Leaf>) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (k, v) in map {
        if is_noise(k) {
            continue;
        }
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Object(_) if budget > 0 => leaves(v, &path, budget - 1, out),
            Value::Object(_) | Value::Null => {}
            Value::Array(items) => {
                if !items.is_empty() && items.iter().all(|i| !i.is_object() && !i.is_array()) {
                    out.push(Leaf {
                        path: format!("{path}[]"),
                        sample: items[0].clone(),
                    });
                }
            }
            scalar => out.push(Leaf {
                path,
                sample: scalar.clone(),
            }),
        }
    }
}

/// The values rule 1 already showed, by the paths it showed them by, so rules 2 to 4 do not
/// repeat them.
///
/// Whole paths, not their top-level ancestors. A path with no dot is a whole top-level value and
/// takes its key out of the walk; a dotted one - `userReference.name`, `config.clusterFunction`,
/// `controllerVm.ipv4.value` - takes exactly the leaf it names. Matching on the ancestor instead
/// would cost the 35 kinds that curate dotted columns their richest sections: a recorded cluster's
/// `config` alone holds 25 leaves under a key one column reaches into for one of them, and §4.5
/// rule 3 asks for a section per top-level object, not a section per uncurated one.
fn shown_by(kind: &Kind) -> Vec<&'static str> {
    let mut out = vec![kind.ext_id_key];
    out.extend(columns_of(kind).iter().map(|c| c.path));
    out
}

/// Labels for the leaves of one section: the last segment of each, grown one segment to its own
/// left - joined by ` · ` - until no two are equal, stopping when a path runs out.
///
/// Leaf segments alone are not unique in practice, which is why this exists. It fires on eight
/// labels of the recorded corpus: a recorded gateway's `deployment` holds four leaves under
/// `managementInterface` - `managementInterface.address.ipv4.value`, its `prefixLength`, and the
/// same pair under `managementInterface.defaultGateway` - and a recorded layer-2 stretch's
/// `localSiteParams` holds two such pairs under `stretchInterfaceIpAddress` and
/// `vpnInterfaceIPAddress`. A recorded host's `controllerVm` and a recorded cluster's `network` hold four
/// `value`s and four `prefixLength`s of their own. `Default gateway · ipv4.value` is twenty-eight
/// cells and spills to a line of its own, which is what the spill rule is for; truncating it to
/// eighteen would put four identical labels in one section.
fn label_leaves(paths: &[&str]) -> Vec<String> {
    let segments: Vec<Vec<&str>> = paths.iter().map(|p| p.split('.').collect()).collect();
    let mut head: Vec<usize> = segments.iter().map(|s| s.len() - 1).collect();
    loop {
        let labels: Vec<String> = segments
            .iter()
            .zip(&head)
            .map(|(segs, from)| label_from(segs, *from))
            .collect();
        let mut grew = false;
        for i in 0..labels.len() {
            if head[i] > 0 && labels.iter().filter(|l| **l == labels[i]).count() > 1 {
                head[i] -= 1;
                grew = true;
            }
        }
        if !grew {
            return labels;
        }
    }
}

/// `{head as a word} · {the rest as the wire writes it}`: `Value`,
/// `External address · ipv4.value`, `Cluster · extId`. The head is a word for a person and the
/// tail is a path, which is exactly what each half is. A fan-out's `[]` is neither: it is the
/// walk's syntax for "every element of", so `dhcpOptions.searchDomains[]` is
/// `Dhcp options · searchDomains`.
fn label_from(segments: &[&str], head: usize) -> String {
    let name = label_of(segments[head]);
    if head + 1 == segments.len() {
        return name;
    }
    let tail = segments[head + 1..].join(".");
    format!("{name} · {}", tail.trim_end_matches("[]"))
}

/// Rule 2's label for a leaf inlined out of a top-level object: `Cluster · extId`,
/// `Impacted entities · clusters`. The same shape a grown rule-3 label has, through the same
/// function, so the two rules cannot drift apart.
fn inline_label(key: &str, leaf: &str) -> String {
    let path = format!("{key}.{leaf}");
    let segments: Vec<&str> = path.split('.').collect();
    label_from(&segments, 0)
}

/// A wire key as a label: `isVtpmEnabled` → `Is vtpm enabled`, `macAddress` → `MAC address`,
/// `ipv4Config` → `IPv4 config`, `capabilities[]` → `Capabilities`.
fn label_of(segment: &str) -> String {
    words::sentence_case(&screaming(segment.trim_end_matches("[]")))
}

/// `powerState` → `POWER_STATE`, `minimumAHVVersion` → `MINIMUM_AHV_VERSION`. The generator's
/// `header_for` with `_` where that writes a space; not shared with it, because it lives in
/// `xtask` and `core` cannot depend on `xtask`. The acronym casing is `words::sentence_case`'s,
/// so `MAC address` in one view and `Mac address` in another is not a state this can reach.
fn screaming(prop: &str) -> String {
    let chars: Vec<char> = prop.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        let prev_lower =
            i > 0 && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());
        // `VMs`: an uppercase run ending in a plural `s` is one word, not an acronym boundary.
        let plural_s = chars.get(i + 1) == Some(&'s')
            && chars.get(i + 2).is_none_or(|n| !n.is_ascii_lowercase());
        let acronym_end = i > 0
            && chars[i - 1].is_ascii_uppercase()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase())
            && !plural_s;
        if c.is_ascii_uppercase() && (prev_lower || acronym_end) {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// The `ColumnKind` a fallback value is drawn with, from the JSON value plus the key name.
///
/// Thirty lines, and deliberately **not** shared with the generator's schema-driven inference:
/// the input is a value, not a schema, and one abstraction over both would fit neither.
fn infer(key: &str, v: &Value) -> ColumnKind {
    let last = key.trim_end_matches("[]").rsplit('.').next().unwrap_or(key);
    let percent = last.ends_with("Percentage") || last.ends_with("Percent");
    match v {
        Value::Bool(_) => ColumnKind::Bool,
        Value::Number(n) if n.is_u64() || n.is_i64() => {
            if last.ends_with("Bytes") {
                ColumnKind::Bytes
            } else if last.ends_with("Secs") || last.ends_with("Seconds") {
                ColumnKind::Duration
            } else if last.ends_with("Usecs") {
                ColumnKind::Micros
            } else if percent {
                ColumnKind::Percent
            } else {
                ColumnKind::Text
            }
        }
        // A float percentage is one spec revision away, and `Percent` already renders one.
        Value::Number(_) if percent => ColumnKind::Percent,
        Value::String(s) if cell::is_rfc3339(s) => ColumnKind::Timestamp,
        Value::String(s) if cell::is_uuid(s) => ColumnKind::Reference,
        Value::String(s) if is_screaming(s) => ColumnKind::Enum,
        _ => ColumnKind::Text,
    }
}

/// `POWERED_OFF`, `ON`: uppercase letters, digits and `_`, with at least one letter. What the v4
/// API writes an enum as.
fn is_screaming(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// One fallback field: the label as given, and the value read back through the same renderer a
/// table cell goes through, so `8 GiB` is `8 GiB` in both.
fn field(
    label: String,
    path: &str,
    sample: &Value,
    raw: &Value,
    names: &Names,
    now: SystemTime,
) -> (String, Rendered) {
    (
        label,
        cell::render_json(path, infer(path, sample), raw, names, now),
    )
}

/// Rule 4's lines: the first three elements, one each.
///
/// Per element: gather its leaf scalars depth-first to [`ELEMENT_DEPTH`], drop the noise keys,
/// order them `name`, `extId`, then wire order, and take the first [`ELEMENT_SCALARS`], joined
/// by two spaces. What is left over is `… and n more`; the elements past the third are already
/// counted by the section's own title, so there is no second marker for them.
///
/// "Left over" is what the line does not show, not what did not fit: a leaf inside the window
/// that rendered `-` is dropped from the text and counted here, because a marker that says four
/// of ten when six are missing is worse than no marker. An element that showed *nothing* keeps
/// no marker at all and falls through to the dim `-` below - a marker standing alone over an
/// empty line is the title-over-blank-lines this rule exists to avoid.
///
/// The label is the element's own variant tag, drawn when it differs from the line above and
/// left blank when it repeats. Both halves are measured: the recorded VM's `disks[]` carries
/// `$objectType` on its first element only, and the recording object store's `publicNetworkIps[]`
/// carries it on every one. A tag identifies what the array holds, so the news is the tag
/// *changing*: three lines labelled `IP address` say nothing the first does, and a tag that
/// changes and changes back is news again.
fn elements(items: &[Value], names: &Names, now: SystemTime) -> Vec<(String, Rendered)> {
    let mut out: Vec<(String, Rendered)> = Vec::new();
    let mut previous = String::new();
    for element in items.iter().take(ELEMENTS) {
        let mut sub = Vec::new();
        leaves(element, "", ELEMENT_DEPTH, &mut sub);
        sub.sort_by_key(|l| match l.path.rsplit('.').next().unwrap_or(&l.path) {
            "name" => 0,
            "extId" => 1,
            _ => 2,
        });
        let mut text = String::new();
        let mut shown = 0;
        for leaf in sub.iter().take(ELEMENT_SCALARS) {
            let r = cell::render_json(
                &leaf.path,
                infer(&leaf.path, &leaf.sample),
                element,
                names,
                now,
            );
            if r.text == "-" {
                continue;
            }
            if !text.is_empty() {
                text.push_str("  ");
            }
            text.push_str(&r.text);
            shown += 1;
        }
        let extra = sub.len() - shown;
        if shown > 0 && extra > 0 {
            text.push_str(&format!("  … and {extra} more"));
        }
        // An element with nothing to say at this depth is the same absence every empty cell is,
        // so a section whose every line is one is dropped by [`push_if_any`] rather than drawn
        // as a title over blank lines. A recorded `iam.authz.AuthorizationPolicy` is the case:
        // its `entities[]` holds one `entityFilter` whose only scalar is four levels down, in a
        // filter map that the raw view (`Y`) is the place to read.
        let value = if text.is_empty() {
            Rendered {
                text: "-".to_string(),
                dim: true,
            }
        } else {
            Rendered { text, dim: false }
        };
        let tag = element
            .get(OBJECT_TYPE)
            .and_then(Value::as_str)
            .map(|t| label_of(short_type(t)))
            .unwrap_or_default();
        let label = if tag == previous {
            String::new()
        } else {
            tag.clone()
        };
        previous = tag;
        out.push((label, value));
    }
    out
}
