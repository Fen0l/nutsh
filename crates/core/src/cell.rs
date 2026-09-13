//! One table cell: a column's value from an entity, rendered for humans.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nutsh_catalog::{Column, ColumnKind};
use nutsh_prism::Entity;
use serde_json::Value;

/// extId → display name, filled by the store from every row it sees.
#[derive(Debug, Default, Clone)]
pub struct Names(HashMap<String, String>);

impl Names {
    /// Remembers `name` as the display name for `ext_id`, overwriting any prior name.
    pub fn insert(&mut self, ext_id: String, name: String) {
        self.0.insert(ext_id, name);
    }

    /// The display name remembered for `ext_id`, if any.
    pub fn get(&self, ext_id: &str) -> Option<&str> {
        self.0.get(ext_id).map(String::as_str)
    }

    /// Whether no extId-to-name entries are remembered.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every extId-to-name pair, for the cache writer.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// One table cell: the text, and whether it is drawn dim.
///
/// Dim is not decoration. It is the difference between a value and the absence of one: an empty
/// cell, an age (context rather than content) and a reference whose name is not known are all
/// things the eye should skip over on the way to the row's actual news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub dim: bool,
}

/// A cell measures as its text. The width rules take `AsRef<str>` and care about nothing else,
/// so a measurer can borrow the cells the renderer already allocated rather than being handed a
/// second copy of them stripped of the flag.
impl AsRef<str> for Rendered {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

/// The cell text for `column` of `entity`, and whether it is dim. `now` anchors ages.
///
/// The one renderer: every table, every page pane, the Contexts screen's rows and the detail
/// pane's fields go through it, so one change here moves every frame.
pub fn render_cell(column: &Column, entity: &Entity, names: &Names, now: SystemTime) -> Rendered {
    render_value(column.path, column.kind, entity, names, now)
}

/// The same cell, from a path and a kind rather than a `Column`.
///
/// It exists because a composed detail field's path may be **owned**: `nutsh_core::detail`'s
/// fallback walks paths out of the entity at render time, and `Column::path` is `&'static str`,
/// so packing one into a `Column` would mean leaking a `String` for every field of every frame.
pub fn render_value(
    path: &str,
    kind: ColumnKind,
    entity: &Entity,
    names: &Names,
    now: SystemTime,
) -> Rendered {
    render_json(path, kind, &entity.raw, names, now)
}

/// The same cell over bare JSON: what a fallback detail's array section renders an element with,
/// since an element of `disks[]` is not an entity of any kind and has no `Kind` to be one of.
///
/// Crate-visible: `nutsh_core::detail` is its only caller, and a renderer that takes a bare
/// `Value` is not something a view should reach for while `render_cell` and `render_value` are
/// what a view has an entity for.
pub(crate) fn render_json(
    path: &str,
    kind: ColumnKind,
    raw: &Value,
    names: &Names,
    now: SystemTime,
) -> Rendered {
    let values = crate::path::get(raw, path);
    // A fan-out that matched nothing is "-", not 0: the path found no array to count.
    if values.is_empty() {
        return empty();
    }
    if kind == ColumnKind::Count {
        // One array value: its length. Several (a fan-out): how many. `0` is `0` and not `-`:
        // "no disks" is a fact, and "the path found nothing" is the absence of one.
        let n = match values.as_slice() {
            [Value::Array(items)] => items.len(),
            many => many.len(),
        };
        return lit(n.to_string());
    }
    // Render each, drop the empties, collapse duplicates preserving order.
    let mut parts: Vec<Rendered> = Vec::new();
    for v in values {
        let r = scalar(kind, v, names, now);
        if r.text.is_empty() || r.text == "-" {
            continue;
        }
        if parts.iter().any(|p| p.text == r.text) {
            continue;
        }
        parts.push(r);
    }
    let mut parts = parts.into_iter();
    let Some(first) = parts.next() else {
        return empty();
    };
    let more = parts.len();
    // A `Bool` fan-out joins its marks instead of counting them. The distinct values of a flag
    // are `✓` and `✗` and nothing else, so `+1` can only ever mean "and one that is not", where
    // it reads as "and one more of these" - a count that carries no information and inverts the
    // sentence. `✓✗` is two cells and says what the array holds.
    if kind == ColumnKind::Bool {
        let mut text = first.text;
        for p in parts {
            text.push_str(&p.text);
        }
        return Rendered {
            text,
            dim: first.dim,
        };
    }
    Rendered {
        text: if more == 0 {
            first.text
        } else {
            format!("{} +{more}", first.text)
        },
        // The first surviving part is what the cell shows; `+n` counts what it does not.
        dim: first.dim,
    }
}

/// The `String` wrapper the sorter and the older tests use. `table::order`'s sort key is the
/// text and nothing else, so the flag costs it nothing.
pub fn render(column: &Column, entity: &Entity, names: &Names, now: SystemTime) -> String {
    render_cell(column, entity, names, now).text
}

/// The one dim `-` every empty cell is: a missing path, a `null`, an empty string, and a
/// fan-out whose every value was one of those.
fn empty() -> Rendered {
    Rendered {
        text: "-".to_string(),
        dim: true,
    }
}

/// A value, drawn in the row's own tint.
fn lit(text: String) -> Rendered {
    Rendered { text, dim: false }
}

fn scalar(kind: ColumnKind, v: &Value, names: &Names, now: SystemTime) -> Rendered {
    match kind {
        ColumnKind::Bytes => lit(v.as_u64().map(bytes).unwrap_or_else(|| plain(v))),
        // Not `plain` on a miss, unlike `Bytes`: the column is six cells wide and a word in it
        // would be cut to nonsense, so an unreadable duration says `-` like any other cell the
        // view could not resolve.
        ColumnKind::Duration => match v.as_u64() {
            Some(secs) => lit(span(secs)),
            None => empty(),
        },
        // An age is dim wherever it is drawn, parsed or not: it is context, not content.
        ColumnKind::Timestamp => Rendered {
            text: v
                .as_str()
                .and_then(parse_rfc3339)
                .map(|t| age(t, now))
                .unwrap_or_else(|| plain(v)),
            dim: true,
        },
        ColumnKind::Micros => Rendered {
            text: match v.as_u64() {
                Some(us) => age(UNIX_EPOCH + Duration::from_micros(us), now),
                None => "-".to_string(),
            },
            dim: true,
        },
        // `as_f64` behind `as_i64`, because `is_percent_name` accepts `number` as well as
        // `integer`: a float-typed percentage is reachable by construction, and `40.5` with no
        // `%` on it is a number the column does not explain.
        ColumnKind::Percent => lit(v
            .as_i64()
            .map(|n| format!("{n}%"))
            .or_else(|| v.as_f64().map(|n| format!("{n:.0}%")))
            .unwrap_or_else(|| plain(v))),
        ColumnKind::Bool => lit(match v.as_bool() {
            Some(true) => "✓".to_string(),
            Some(false) => "✗".to_string(),
            None => plain(v),
        }),
        ColumnKind::Status => lit(shout(&plain(v))),
        ColumnKind::Enum => lit(sentence(&plain(v))),
        ColumnKind::Reference => reference(v, names),
        // `Ip` renders exactly as `Text`; its whole job is `table::rule`.
        ColumnKind::Ip | ColumnKind::Text => lit(plain(v)),
        ColumnKind::Count => {
            unreachable!("a Count cell never reaches scalar: render_cell returns the length first")
        }
    }
}

/// A `Status` word: strip a leading `$`, `_` to space, uppercase.
///
/// Upper-casing does not break the row tint. `status::role` normalises by trimming, stripping a
/// leading `$`, uppercasing, and mapping `-` and space to `_`, so `POWERED OFF` normalises back
/// to `POWERED_OFF` and finds the role the wire word did - and so do the per-kind overrides in
/// `curated.toml`, which are normalised the same way. `render_rows` calls
/// `status::role_in(kind, row[i])` on the *rendered* text, which is exactly why
/// `shouting_a_status_does_not_change_its_role` exists.
fn shout(word: &str) -> String {
    if !is_token(word) {
        return word.to_string();
    }
    word.strip_prefix('$')
        .unwrap_or(word)
        .replace('_', " ")
        .to_uppercase()
}

/// Any other `Enum` word: sentence case, with the acronyms the catalog already knows.
fn sentence(word: &str) -> String {
    if !is_token(word) {
        return word.to_string();
    }
    nutsh_catalog::words::sentence_case(word.strip_prefix('$').unwrap_or(word))
}

/// One enum-shaped token: letters, digits, `_` and `$`, and nothing else.
///
/// `platformData.connectivityStatus` is an unconstrained string in
/// `specs/multidomain/v4.3.yaml`, so a Prism Central may put a sentence there. Anything with a
/// space in it was written by a person and is left as it was written.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// What a Prism Central writes for "no project" and "no owner".
const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// §4.4: the name when it is known; the first eight characters, **dim**, when it is not and the
/// value is UUID-shaped; the value whole when it is not UUID-shaped, because some older shapes
/// carry a name in a `*Reference` string.
///
/// "Not yet known" and "never knowable" are the same cell, because they look identical from
/// inside one: a cell that guessed would be wrong on a slow Prism Central. The full identifier
/// is never lost - `y` opens the raw entity, extIds and all, and that is one keystroke.
fn reference(v: &Value, names: &Names) -> Rendered {
    let id = plain(v);
    // The nil UUID is the absence of a reference, so it is the same dim `-` every other absence
    // is. First, ahead of the cache: an `00000000` stub would read as a reference to something
    // the warm-up has not reached yet, and a reader would go looking for the entity it names.
    if id == NIL_UUID {
        return empty();
    }
    if let Some(name) = names.get(&id) {
        return lit(name.to_string());
    }
    if is_uuid(&id) {
        return Rendered {
            text: id.chars().take(8).collect(),
            dim: true,
        };
    }
    lit(id)
}

/// `8-4-4-4-12` hexadecimal. Nutanix writes this one literal pattern on every identifier
/// property in the specs, so it is the whole test.
pub(crate) fn is_uuid(s: &str) -> bool {
    let mut parts = s.split('-');
    for len in [8usize, 4, 4, 4, 12] {
        match parts.next() {
            Some(p) if p.len() == len && p.chars().all(|c| c.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => without_bidi(s),
        Value::Null => "-".to_string(),
        other => other.to_string(),
    }
}

/// Text out of Prism Central without the bidirectional overrides and isolates.
///
/// A cell is one run of characters in a line shared with every other column, so `U+202E` in a
/// VM name does not merely reverse that name: it reverses everything drawn after it, and the
/// line stops saying what the table says. Names cannot be trusted to be well-formed - they are
/// whatever somebody typed into the PC - so the controls are dropped rather than balanced.
fn without_bidi(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect()
}

/// `1.5 GiB`: binary units, one decimal, `.0` dropped.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = n as f64;
    let mut unit = 0;
    // 1023.95 rather than 1024.0: a value that would display as "1024.0" after rounding to
    // one decimal must bump the unit instead.
    while value >= 1023.95 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{n} B");
    }
    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", UNITS[unit])
}

/// Whether `s` is an RFC-3339 instant. One parser, so a `Timestamp` a schema declared and a
/// `Timestamp` the fallback inferred agree about what a date looks like.
pub(crate) fn is_rfc3339(s: &str) -> bool {
    parse_rfc3339(s).is_some()
}

fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    let unix = t.unix_timestamp();
    Some(if unix >= 0 {
        UNIX_EPOCH + Duration::from_secs(unix as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(unix.unsigned_abs())
    })
}

/// `45s`, `3m`, `2h`, `5d`, `1y`: the one spelling of a length of time, shared by `Timestamp`'s
/// and `Micros`'s ages and by `Duration`'s value, so the columns never disagree about what
/// ninety minutes looks like.
///
/// Public for the same reason [`age`] is: a length of time drawn outside a cell - the elapsed
/// time of a table's walk in its title - is spelled here or it is spelled twice.
pub fn span(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        // 365 days, not 365.25: the recording's images read `700d` today, and `1y` is a thing a
        // person can hold in their head.
        s if s < 31_536_000 => format!("{}d", s / 86_400),
        s => format!("{}y", s / 31_536_000),
    }
}

/// `45s`, `3m`, `2h`, `5d`, `1y`; a timestamp in the future is `0s`.
///
/// Public for the journal view, which shows a `WHEN` beside table rows carrying `Timestamp`
/// cells: two spellings of "how long ago" on one screen is one too many.
pub fn age(t: SystemTime, now: SystemTime) -> String {
    span(now.duration_since(t).map(|d| d.as_secs()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nutsh_catalog::kind;
    use serde_json::json;

    fn col(path: &'static str, kind: ColumnKind) -> Column {
        Column {
            header: "H",
            path,
            kind,
        }
    }

    fn entity(raw: Value) -> Entity {
        Entity::new(kind("vmm.ahv.config.Vm").unwrap(), raw, None)
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn text_enum_bool_and_missing() {
        let e = entity(json!({"name": "web-01", "powerState": "ON", "isAgentVm": false}));
        let names = Names::default();
        let now = at(0);
        assert_eq!(
            render(&col("name", ColumnKind::Text), &e, &names, now),
            "web-01"
        );
        assert_eq!(
            render(&col("powerState", ColumnKind::Enum), &e, &names, now),
            "On"
        );
        assert_eq!(
            render(&col("isAgentVm", ColumnKind::Bool), &e, &names, now),
            "✗"
        );
        assert_eq!(render(&col("nope", ColumnKind::Text), &e, &names, now), "-");
    }

    /// A name that carries a bidirectional override would reorder the rest of the line, so
    /// the overrides never reach the frame; everything else about the name survives.
    #[test]
    fn bidi_controls_are_dropped_from_text() {
        let names = Names::default();
        let now = at(0);
        let e = entity(json!({
            "name": "web-\u{202E}10-db\u{202C}",
            "description": "\u{2066}a\u{2067}b\u{2068}c\u{2069}d",
        }));
        assert_eq!(
            render(&col("name", ColumnKind::Text), &e, &names, now),
            "web-10-db"
        );
        assert_eq!(
            render(&col("description", ColumnKind::Text), &e, &names, now),
            "abcd"
        );
        // Marks and joiners are ordinary text and stay: only the overrides are a problem.
        let e = entity(json!({"name": "\u{200E}web\u{200F}-01"}));
        assert_eq!(
            render(&col("name", ColumnKind::Text), &e, &names, now),
            "\u{200E}web\u{200F}-01"
        );
    }

    #[test]
    fn bytes_are_binary_units_with_one_decimal() {
        let names = Names::default();
        let now = at(0);
        for (n, want) in [
            (0u64, "0 B"),
            (512, "512 B"),
            (1024, "1 KiB"),
            (1536, "1.5 KiB"),
            (8_589_934_592, "8 GiB"),
            (53_687_091_200, "50 GiB"),
            (1_649_267_441_664, "1.5 TiB"),
            (1023, "1023 B"),
            (1_048_575, "1 MiB"),
            (1_099_511_627_775, "1 TiB"),
            (4_503_599_627_370_496, "4 PiB"),
            (u64::MAX, "16 EiB"),
        ] {
            let e = entity(json!({"memorySizeBytes": n}));
            assert_eq!(
                render(&col("memorySizeBytes", ColumnKind::Bytes), &e, &names, now),
                want,
                "{n}"
            );
        }
    }

    #[test]
    fn timestamps_render_as_age() {
        let names = Names::default();
        let e = entity(
            json!({"createTime": "2026-09-05T10:00:00Z", "updateTime": "2026-09-05T10:00:00.250+02:00"}),
        );
        let base = at(1_788_602_400); // 2026-09-05T10:00:00Z
        assert_eq!(
            render(&col("createTime", ColumnKind::Timestamp), &e, &names, base),
            "0s"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(45)
            ),
            "45s"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(59)
            ),
            "59s"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(60)
            ),
            "1m"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(3 * 60 + 5)
            ),
            "3m"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(3599)
            ),
            "59m"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(3600)
            ),
            "1h"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(2 * 3600 + 59 * 60)
            ),
            "2h"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(86399)
            ),
            "23h"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(86400)
            ),
            "1d"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base + Duration::from_secs(5 * 86400)
            ),
            "5d"
        );
        // +02:00 at 10:00 local is 08:00Z, two hours before base.
        assert_eq!(
            render(&col("updateTime", ColumnKind::Timestamp), &e, &names, base),
            "2h"
        );
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &e,
                &names,
                base - Duration::from_secs(10)
            ),
            "0s",
            "future dates never go negative"
        );
        let bad = entity(json!({"createTime": "yesterday"}));
        assert_eq!(
            render(
                &col("createTime", ColumnKind::Timestamp),
                &bad,
                &names,
                base
            ),
            "yesterday"
        );
    }

    /// A `Status` shouts and any other `Enum` is sentence case, so the two do not render alike:
    /// the difference is in the words themselves, not only in the style.
    #[test]
    fn status_shouts_where_an_enum_is_sentence_case() {
        let e = entity(json!({"powerState": "ON"}));
        let names = Names::default();
        let now = at(0);
        assert_eq!(
            render(&col("powerState", ColumnKind::Status), &e, &names, now),
            "ON"
        );
        assert_eq!(
            render(&col("powerState", ColumnKind::Enum), &e, &names, now),
            "On"
        );
    }

    /// An RPO is `6h`, not `21600`. Anything that is not a non-negative integer is `-`: a
    /// `Duration` column is six cells wide, and a column six cells wide that prints a word is
    /// a column that lies about what it holds.
    #[test]
    fn durations_are_spans_not_counts_of_seconds() {
        let names = Names::default();
        let now = at(0);
        for (secs, want) in [
            (0u64, "0s"),
            (45, "45s"),
            (60, "1m"),
            (900, "15m"),
            (3600, "1h"),
            (21_600, "6h"),
            (86_400, "1d"),
            (172_800, "2d"),
        ] {
            let e = entity(json!({"rpo": secs}));
            assert_eq!(
                render(&col("rpo", ColumnKind::Duration), &e, &names, now),
                want,
                "{secs}"
            );
        }
        let text = entity(json!({"rpo": "soon"}));
        assert_eq!(
            render(&col("rpo", ColumnKind::Duration), &text, &names, now),
            "-"
        );
        let negative = entity(json!({"rpo": -1}));
        assert_eq!(
            render(&col("rpo", ColumnKind::Duration), &negative, &names, now),
            "-"
        );
        let missing = entity(json!({}));
        assert_eq!(
            render(&col("rpo", ColumnKind::Duration), &missing, &names, now),
            "-"
        );
    }

    #[test]
    fn count_reference_and_multi_valued() {
        let mut names = Names::default();
        names.insert("c1".into(), "lab-cluster".into());
        let now = at(0);
        let e = entity(json!({
            "disks": [{}, {}, {}],
            "cluster": {"extId": "c1"},
            "host": {"extId": "0006158a-2f0d-4d5a-8e2d-000000000010"},
            "nics": [{"networkInfo": {"ipv4Config": {"ipAddress": {"value": "10.0.0.1"}}}},
                     {"networkInfo": {"ipv4Config": {"ipAddress": {"value": "10.0.0.2"}}}}]
        }));
        assert_eq!(
            render(&col("disks", ColumnKind::Count), &e, &names, now),
            "3"
        );
        assert_eq!(
            render(&col("nope", ColumnKind::Count), &e, &names, now),
            "-"
        );
        assert_eq!(
            render(
                &col("cluster.extId", ColumnKind::Reference),
                &e,
                &names,
                now
            ),
            "lab-cluster"
        );
        assert_eq!(
            render(&col("host.extId", ColumnKind::Reference), &e, &names, now),
            "0006158a"
        );
        assert_eq!(
            render(
                &col(
                    "nics[].networkInfo.ipv4Config.ipAddress.value",
                    ColumnKind::Text
                ),
                &e,
                &names,
                now
            ),
            "10.0.0.1 +1"
        );
    }

    fn dim(path: &'static str, kind: ColumnKind, raw: Value, now: SystemTime) -> bool {
        render_cell(&col(path, kind), &entity(raw), &Names::default(), now).dim
    }

    fn text(path: &'static str, kind: ColumnKind, raw: Value, now: SystemTime) -> String {
        render_cell(&col(path, kind), &entity(raw), &Names::default(), now).text
    }

    /// The three empties are one cell: `-`, dim. An empty name is not a name.
    #[test]
    fn every_empty_is_one_dim_dash() {
        let now = at(0);
        for raw in [json!({}), json!({"name": null}), json!({"name": ""})] {
            let r = render_cell(
                &col("name", ColumnKind::Text),
                &entity(raw.clone()),
                &Names::default(),
                now,
            );
            assert_eq!((r.text.as_str(), r.dim), ("-", true), "{raw}");
        }
        // A fan-out whose every value is empty is the same cell.
        let r = render_cell(
            &col("nics[].ip", ColumnKind::Ip),
            &entity(json!({"nics": [{"ip": null}, {"ip": ""}]})),
            &Names::default(),
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("-", true));
    }

    /// A `Status` shouts, an `Enum` is sentence case, and both strip a leading `$`.
    #[test]
    fn status_shouts_and_enum_is_sentence_case() {
        let now = at(0);
        for (wire, status, enumerated) in [
            ("POWERED_OFF", "POWERED OFF", "Powered off"),
            ("$UNKNOWN", "UNKNOWN", "Unknown"),
            ("normal", "NORMAL", "Normal"),
            ("DISK_IMAGE", "DISK IMAGE", "Disk image"),
            ("AHV", "AHV", "AHV"),
            ("IPV4", "IPV4", "IPv4"),
            ("VM_ANTI_AFFINITY", "VM ANTI AFFINITY", "VM anti affinity"),
            ("PLANNED_FAILOVER", "PLANNED FAILOVER", "Planned failover"),
        ] {
            let raw = json!({ "w": wire });
            assert_eq!(
                text("w", ColumnKind::Status, raw.clone(), now),
                status,
                "{wire}"
            );
            assert_eq!(text("w", ColumnKind::Enum, raw, now), enumerated, "{wire}");
        }
    }

    /// `platformData.connectivityStatus` is an unconstrained string in
    /// `specs/multidomain/v4.3.yaml`, so a Prism Central may put a sentence there. Shouting a
    /// sentence is shouting at the user about something the program does not understand.
    #[test]
    fn only_an_enum_shaped_token_is_recased() {
        let now = at(0);
        let raw = json!({"w": "Connected to Nutanix Central"});
        assert_eq!(
            text("w", ColumnKind::Status, raw.clone(), now),
            "Connected to Nutanix Central"
        );
        assert_eq!(
            text("w", ColumnKind::Enum, raw, now),
            "Connected to Nutanix Central"
        );
    }

    /// A flag is a mark. Never `true`/`false`, and never `-` for false: a false flag is an
    /// answer, and a dash is the absence of one.
    #[test]
    fn bools_are_marks() {
        let now = at(0);
        assert_eq!(text("b", ColumnKind::Bool, json!({"b": true}), now), "✓");
        assert_eq!(text("b", ColumnKind::Bool, json!({"b": false}), now), "✗");
        assert!(!dim("b", ColumnKind::Bool, json!({"b": false}), now));
        assert_eq!(text("b", ColumnKind::Bool, json!({}), now), "-");
    }

    /// A percentage carries its sign; anything that is not a number falls back to the text.
    #[test]
    fn percents_carry_their_sign() {
        let now = at(0);
        assert_eq!(text("p", ColumnKind::Percent, json!({"p": 40}), now), "40%");
        assert_eq!(text("p", ColumnKind::Percent, json!({"p": 0}), now), "0%");
        assert_eq!(
            text("p", ColumnKind::Percent, json!({"p": 100}), now),
            "100%"
        );
        assert_eq!(
            text("p", ColumnKind::Percent, json!({"p": "n/a"}), now),
            "n/a"
        );
        // A float still carries the sign: `is_percent_name` accepts `number`, so this shape is
        // one spec revision away.
        assert_eq!(
            text("p", ColumnKind::Percent, json!({"p": 40.5}), now),
            "40%"
        );
        assert_eq!(
            text("p", ColumnKind::Percent, json!({"p": 99.6}), now),
            "100%"
        );
    }

    /// Microseconds and an RFC 3339 timestamp are the same instant expressed two ways, and
    /// they must render identically; both carry the dim flag.
    ///
    /// The flag is a value on `Rendered` and nothing reads it yet - `ui.rs` still dims a cell by
    /// `ColumnKind::Timestamp` - so this asserts what `render_cell` answers, not what the frame
    /// draws.
    #[test]
    fn micros_and_timestamps_agree_on_the_same_instant() {
        let base = at(1_788_602_400); // 2026-09-05T10:00:00Z
        let now = base + Duration::from_secs(5 * 86_400);
        let micros = json!({"t": 1_788_602_400_000_000u64});
        let rfc = json!({"t": "2026-09-05T10:00:00Z"});
        assert_eq!(text("t", ColumnKind::Micros, micros.clone(), now), "5d");
        assert_eq!(text("t", ColumnKind::Timestamp, rfc.clone(), now), "5d");
        assert!(dim("t", ColumnKind::Micros, micros, now));
        assert!(dim("t", ColumnKind::Timestamp, rfc, now));
        // Seven cells: a raw microsecond count would be cut to nonsense.
        assert_eq!(
            text("t", ColumnKind::Micros, json!({"t": "soon"}), now),
            "-"
        );
    }

    /// Over 365 days an age reads in years: the recording's images show `700d`, and `1y` is a thing a
    /// person can hold in their head. A `Duration` takes the same step.
    #[test]
    fn a_year_is_a_year() {
        let base = at(1_000_000_000);
        let rfc = json!({"t": "2001-09-09T01:46:40Z"});
        for (secs, want) in [
            (364 * 86_400u64, "364d"),
            (365 * 86_400, "1y"),
            (800 * 86_400, "2y"),
        ] {
            assert_eq!(
                text(
                    "t",
                    ColumnKind::Timestamp,
                    rfc.clone(),
                    base + Duration::from_secs(secs)
                ),
                want,
                "{secs}"
            );
        }
        assert_eq!(
            text("d", ColumnKind::Duration, json!({"d": 400 * 86_400}), base),
            "1y"
        );
    }

    /// Several values: render each, drop the empties, collapse duplicates in order, then the
    /// first alone or the first with a count of what it stands for - except a `Bool`, whose two
    /// marks are joined, because a count of flags says the opposite of what it means.
    #[test]
    fn several_values_collapse_to_the_first_plus_a_count() {
        let now = at(0);
        let raw = json!({
            "nics": [{"ip": "192.0.2.135"}, {"ip": "192.0.2.102"}],
            "rpo": [{"s": 3600}, {"s": 3600}],
            "flags": [{"b": true}, {"b": false}],
            "same": [{"b": true}, {"b": true}],
            "sparse": [{"ip": null}, {"ip": "10.0.0.1"}, {"ip": ""}]
        });
        assert_eq!(
            text("nics[].ip", ColumnKind::Ip, raw.clone(), now),
            "192.0.2.135 +1"
        );
        assert_eq!(
            text("rpo[].s", ColumnKind::Duration, raw.clone(), now),
            "1h"
        );
        assert_eq!(
            text("flags[].b", ColumnKind::Bool, raw.clone(), now),
            "✓✗",
            "`✓ +1` would read as one primary and one more primary"
        );
        assert_eq!(
            text("same[].b", ColumnKind::Bool, raw.clone(), now),
            "✓",
            "duplicates still collapse before the join"
        );
        assert_eq!(
            text("sparse[].ip", ColumnKind::Ip, raw, now),
            "10.0.0.1",
            "the empties are dropped before the count"
        );
    }

    /// §4.4, the four situations a `Reference` cell can be in.
    #[test]
    fn references_resolve_stub_or_show_a_name_whole() {
        let now = at(0);
        let mut names = Names::default();
        names.insert(
            "0006158a-2f0d-4d5a-8e2d-000000000010".into(),
            "cluster-a".into(),
        );
        let known = json!({"r": "0006158a-2f0d-4d5a-8e2d-000000000010"});
        let unknown = json!({"r": "38521854-0f6a-4b58-9f9b-000000000020"});
        let not_a_uuid = json!({"r": "lab-cluster"});

        let r = render_cell(
            &col("r", ColumnKind::Reference),
            &entity(known),
            &names,
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("cluster-a", false));

        let r = render_cell(
            &col("r", ColumnKind::Reference),
            &entity(unknown),
            &names,
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("38521854", true), "the dim stub");

        // Some older shapes carry a name in a `*Reference` string: shown whole, not dim, not
        // truncated to eight.
        let r = render_cell(
            &col("r", ColumnKind::Reference),
            &entity(not_a_uuid),
            &names,
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("lab-cluster", false));

        let r = render_cell(
            &col("nope", ColumnKind::Reference),
            &entity(json!({})),
            &names,
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("-", true));
    }

    /// The one test that proves upper-casing a badge did not silently change the row colours:
    /// `status::role_in` normalises by trimming, stripping a leading `$`, uppercasing and
    /// folding `-` and space to `_`, so the rendered word and the wire word must find the same
    /// role for every word in the vocabulary.
    #[test]
    fn shouting_a_status_does_not_change_its_role() {
        let now = at(0);
        let vm = kind("vmm.ahv.config.Vm").unwrap();
        let host = kind("clustermgmt.config.Host").unwrap();
        for word in [
            "ON",
            "OFF",
            "POWERED_ON",
            "POWERED_OFF",
            "NORMAL",
            "normal",
            "HEALTHY",
            "CRITICAL",
            "WARNING",
            "INFO",
            "FAILED",
            "ERROR",
            "SUCCEEDED",
            "RUNNING",
            "QUEUED",
            "PENDING",
            "IN_PROGRESS",
            "COMPLETE",
            "IN_MAINTENANACE",
            "ENTERING_MAINTENANCE",
            "$UNKNOWN",
            "$REDACTED",
            "SUSPENDED",
            "UNDETERMINED",
        ] {
            let rendered = text("s", ColumnKind::Status, json!({"s": word}), now);
            for k in [vm, host] {
                assert_eq!(
                    crate::status::role_in(k, &rendered),
                    crate::status::role_in(k, word),
                    "{word} rendered as {rendered} on {}",
                    k.id
                );
            }
        }
    }

    /// `render_value` is what a composed detail field goes through, and `render_cell` is a
    /// one-line wrapper over it, so a `Bytes` field and a `Bytes` column are the same cell.
    /// It has to exist: `Column::path` is `&'static str`, and a fallback field's path is walked
    /// out of the entity at render time, so it cannot be packed into a `Column` without leaking
    /// a `String`.
    #[test]
    fn render_value_is_render_cell_without_a_column() {
        let e = entity(json!({"memorySizeBytes": 8_589_934_592u64, "powerState": "POWERED_OFF"}));
        let names = Names::default();
        let now = at(0);
        // A runtime path: owned, not `&'static`, which is the whole reason for the function.
        let path = String::from("memorySizeBytes");
        assert_eq!(
            render_value(&path, ColumnKind::Bytes, &e, &names, now),
            render_cell(&col("memorySizeBytes", ColumnKind::Bytes), &e, &names, now)
        );
        assert_eq!(
            render_value("memorySizeBytes", ColumnKind::Bytes, &e, &names, now).text,
            "8 GiB"
        );
        assert_eq!(
            render_value("powerState", ColumnKind::Status, &e, &names, now).text,
            "POWERED OFF"
        );
    }

    /// The nil UUID is how a Prism Central writes "no project" and "no owner": on the hundred
    /// VMs of `crates/mockpc/fixtures-lab/vmm/v4.3/ahv/config/vms.json` it is `project.extId`
    /// on 89 and `ownershipInfo.owner.extId` on 86. An `00000000` stub is a reference to
    /// nothing that looks like a reference to something the cache has not warmed yet, which is
    /// the one confusion the `Reference` rule was written to prevent.
    #[test]
    fn the_nil_uuid_is_a_dash_not_a_stub() {
        let now = at(0);
        let r = render_cell(
            &col("r", ColumnKind::Reference),
            &entity(json!({"r": "00000000-0000-0000-0000-000000000000"})),
            &Names::default(),
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("-", true));
        // A fan-out of nothing but nil references is the same cell: the empties are dropped
        // before the count.
        let r = render_cell(
            &col("v[].extId", ColumnKind::Reference),
            &entity(json!({"v": [
                {"extId": "00000000-0000-0000-0000-000000000000"},
                {"extId": "00000000-0000-0000-0000-000000000000"}
            ]})),
            &Names::default(),
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("-", true));
        // A real reference beside a nil one is the cell: the nil contributes nothing, not a `+1`.
        let r = render_cell(
            &col("v[].extId", ColumnKind::Reference),
            &entity(json!({"v": [
                {"extId": "00000000-0000-0000-0000-000000000000"},
                {"extId": "0006158a-2f0d-4d5a-8e2d-000000000010"}
            ]})),
            &Names::default(),
            now,
        );
        assert_eq!((r.text.as_str(), r.dim), ("0006158a", true));
    }

    /// `span` is public so the walk's elapsed time in the table title is spelled the way every
    /// other length of time in the program is.
    #[test]
    fn span_is_the_one_spelling_of_a_length_of_time() {
        assert_eq!(span(0), "0s");
        assert_eq!(span(3), "3s");
        assert_eq!(span(120), "2m");
        assert_eq!(span(86_400), "1d");
    }
}
