//! How often a kind's views poll: four levels, resolved in one place.
//!
//! One function, because the settings screen prints **where** a value came from and a second
//! implementation of precedence would be a second answer to that question.

use std::time::Duration;

use nutsh_catalog::Kind;
use nutsh_config::{Interval, Refresh, Word};

use crate::contexts::Source;
use crate::scheduler::MIN_INTERVAL;

/// The rungs `ctrl-t` steps through, slowest last. `auto` and `off` are the two ends; the
/// numbers between them are the ones a person actually asks for.
pub const LADDER: &[Interval] = &[
    Interval::Word(Word::Auto),
    Interval::Secs(5),
    Interval::Secs(10),
    Interval::Secs(30),
    Interval::Secs(60),
    Interval::Secs(300),
    Interval::Secs(3_600),
    Interval::Secs(21_600),
    Interval::Secs(43_200),
    Interval::Word(Word::Off),
];

/// How often this kind's views poll, and where that came from.
///
/// Highest first: the session's override, `[refresh.kinds."id"]`, `[refresh.namespaces]`,
/// `[refresh] default`, and the
/// catalog's `poll_secs`. `None` is no schedule at all. Absence is what falls through to the
/// next level; `auto` does not fall through - it **names** the catalog, which is the escape
/// hatch a global default needs, and it reports `Source::Catalog` wherever it was written,
/// because the source column answers "where did this number come from" and the number came from
/// the catalog.
pub fn effective(
    kind: &'static Kind,
    cfg: &Refresh,
    session: Option<Interval>,
) -> (Option<Duration>, Source) {
    if let Some(v) = session {
        return one(kind, v, Source::Session);
    }
    if let Some(v) = cfg.kinds.get(kind.id).copied() {
        return one(kind, v, Source::File);
    }
    if let Some(v) = cfg.namespaces.get(kind.namespace).copied() {
        return one(kind, v, Source::Namespace(kind.namespace));
    }
    if let Some(v) = cfg.default {
        return one(kind, v, Source::File);
    }
    (Some(catalog(kind)), Source::Catalog)
}

fn one(kind: &'static Kind, v: Interval, src: Source) -> (Option<Duration>, Source) {
    match v {
        Interval::Word(Word::Auto) => (Some(catalog(kind)), Source::Catalog),
        Interval::Word(Word::Off) => (None, src),
        // Floored here as well as in `poll_interval`, so the value the screen prints and the
        // value the poller waits are the same number. A hand-edited file is the only way in.
        Interval::Secs(n) => (
            Some(Duration::from_secs(u64::from(n)).max(MIN_INTERVAL)),
            src,
        ),
    }
}

/// The kind's curated rhythm, floored: `Kind::poll_secs` is a `u32` the generator writes and a
/// zero there would be a catalog asking for a busy loop.
fn catalog(kind: &'static Kind) -> Duration {
    Duration::from_secs(u64::from(kind.poll_secs)).max(MIN_INTERVAL)
}

/// The next rung down from what is running now: the first rung **slower** than it, wrapping
/// from `off` back to `auto`. Entering at the first slower rung is what keeps a 3 s kind from
/// spending a press on the 5 s it is nearly already at.
pub fn step(current: Option<Duration>) -> Interval {
    let Some(now) = current else {
        return Interval::Word(Word::Auto);
    };
    LADDER
        .iter()
        .copied()
        .find(|rung| matches!(rung, Interval::Secs(n) if Duration::from_secs(u64::from(*n)) > now))
        .unwrap_or(Interval::Word(Word::Off))
}

/// The rung after this one, by position on the ladder, wrapping from `off` back to `auto`.
///
/// For a row whose value **is** a rung rather than a length of time: `[refresh] default` names
/// one and stands for no kind in particular, so there is no duration to be "the first slower
/// than". `None` is a file that never mentioned it, which is `auto`.
pub fn step_from(current: Option<Interval>) -> Interval {
    let at = current
        .and_then(|v| LADDER.iter().position(|rung| *rung == v))
        .unwrap_or(0);
    LADDER[(at + 1) % LADDER.len()]
}

/// How a rung is drawn: `auto`, `off`, or `cell::span`'s spelling of the seconds. The screen's
/// value column, the palette's completion rows and the flash all print through this, so a rung
/// is spelled once.
pub fn show(v: Interval) -> String {
    match v {
        Interval::Word(Word::Auto) => "auto".to_string(),
        Interval::Word(Word::Off) => "off".to_string(),
        Interval::Secs(n) => crate::cell::span(u64::from(n)),
    }
}

/// `auto`, `off`, or a length of time in the one spelling this program has - `cell::span`'s, so
/// `45`, `45s`, `1m`, `5m` and `12h` all read. Refused, never clamped: a screen showing `0s` in a
/// source column over a poller running at one second would be lying in the one place this work
/// exists to stop lying.
pub fn parse(word: &str) -> Result<Interval, String> {
    let word = word.trim();
    match word {
        "auto" => return Ok(Interval::Word(Word::Auto)),
        "off" => return Ok(Interval::Word(Word::Off)),
        _ => {}
    }
    let (digits, mult) = match word.strip_suffix('h') {
        Some(d) => (d, 3_600u32),
        None => match word.strip_suffix('m') {
            Some(d) => (d, 60u32),
            None => (word.strip_suffix('s').unwrap_or(word), 1u32),
        },
    };
    let n: u32 = digits.parse().map_err(|_| {
        format!("{word:?} is not auto, off, or a length of time like 30s, 5m or 12h")
    })?;
    let secs = n.saturating_mul(mult);
    if u64::from(secs) < MIN_INTERVAL.as_secs() {
        return Err(format!("{}s is the floor", MIN_INTERVAL.as_secs()));
    }
    Ok(Interval::Secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(id: &str) -> &'static nutsh_catalog::Kind {
        nutsh_catalog::kind(id).expect("a catalog kind")
    }
    fn secs(d: Option<Duration>) -> Option<u64> {
        d.map(|d| d.as_secs())
    }

    /// The four levels, highest first, each one asserted against the one below it.
    #[test]
    fn a_namespace_schedules_its_kinds_and_a_kind_of_its_own_still_wins() {
        let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
        let image = nutsh_catalog::kind("vmm.content.Image").expect("Images");
        let host = nutsh_catalog::kind("clustermgmt.config.Host").expect("Hosts");

        let mut cfg = Refresh::default();
        cfg.namespaces
            .insert("vmm".to_string(), Interval::Secs(300));

        // Every kind of the namespace, and nothing outside it.
        assert_eq!(
            effective(vm, &cfg, None),
            (Some(Duration::from_secs(300)), Source::Namespace("vmm"))
        );
        assert_eq!(
            effective(image, &cfg, None),
            (Some(Duration::from_secs(300)), Source::Namespace("vmm"))
        );
        assert_eq!(effective(host, &cfg, None).1, Source::Catalog);

        // A kind of its own outranks its namespace, and the session outranks both.
        cfg.kinds
            .insert("vmm.ahv.config.Vm".to_string(), Interval::Secs(10));
        assert_eq!(
            effective(vm, &cfg, None),
            (Some(Duration::from_secs(10)), Source::File)
        );
        assert_eq!(
            effective(vm, &cfg, Some(Interval::Secs(5))),
            (Some(Duration::from_secs(5)), Source::Session)
        );

        // And a namespace outranks the global default.
        cfg.default = Some(Interval::Secs(60));
        assert_eq!(effective(image, &cfg, None).1, Source::Namespace("vmm"));
        assert_eq!(effective(host, &cfg, None).1, Source::File);
    }

    #[test]
    fn precedence_runs_session_then_kind_then_default_then_the_catalog() {
        let tasks = kind("prism.config.Task");
        let curated = u64::from(tasks.poll_secs);
        let mut cfg = Refresh::default();

        let (every, src) = effective(tasks, &cfg, None);
        assert_eq!((secs(every), src), (Some(curated), Source::Catalog));

        cfg.default = Some(Interval::Secs(60));
        let (every, src) = effective(tasks, &cfg, None);
        assert_eq!(
            (secs(every), src),
            (Some(60), Source::File),
            "the global default"
        );

        cfg.kinds
            .insert(tasks.id.to_string(), Interval::Word(Word::Auto));
        let (every, src) = effective(tasks, &cfg, None);
        assert_eq!(
            (secs(every), src),
            (Some(curated), Source::Catalog),
            "`auto` names the catalog, which is the escape hatch a global default needs"
        );

        let (every, src) = effective(tasks, &cfg, Some(Interval::Secs(10)));
        assert_eq!((secs(every), src), (Some(10), Source::Session));
    }

    /// `off` at each level answers `None` and consults nothing below it.
    #[test]
    fn off_at_any_level_is_no_schedule_at_all() {
        let vms = kind("vmm.ahv.config.Vm");
        let mut cfg = Refresh {
            default: Some(Interval::Word(Word::Off)),
            ..Refresh::default()
        };
        assert_eq!(effective(vms, &cfg, None), (None, Source::File));

        cfg.kinds
            .insert(vms.id.to_string(), Interval::Word(Word::Off));
        cfg.default = Some(Interval::Secs(30));
        assert_eq!(effective(vms, &cfg, None), (None, Source::File));

        assert_eq!(
            effective(vms, &Refresh::default(), Some(Interval::Word(Word::Off))),
            (None, Source::Session)
        );
    }

    /// The floor is refused, never clamped, and the message cites the constant rather than
    /// spelling the number a second time.
    #[test]
    fn parse_takes_the_one_spelling_of_a_length_of_time_and_refuses_the_rest() {
        assert_eq!(parse("auto"), Ok(Interval::Word(Word::Auto)));
        assert_eq!(parse("off"), Ok(Interval::Word(Word::Off)));
        assert_eq!(parse("45"), Ok(Interval::Secs(45)));
        assert_eq!(parse("1h"), Ok(Interval::Secs(3_600)), "hours read too");
        assert_eq!(parse("12h"), Ok(Interval::Secs(43_200)));
        assert_eq!(parse("45s"), Ok(Interval::Secs(45)));
        assert_eq!(parse("1m"), Ok(Interval::Secs(60)), "stored as seconds");
        assert_eq!(parse("5m"), Ok(Interval::Secs(300)));
        for bad in ["0", "0s", "-1", "1x", "", "sometimes"] {
            let e = parse(bad).expect_err(bad);
            assert!(!e.is_empty(), "{bad} must say why");
        }
        assert!(
            parse("0").unwrap_err().contains("floor"),
            "the floor names itself"
        );
    }

    /// One rung down, entered at the first rung *slower* than what is running now, wrapping
    /// through `off` back to `auto`.
    #[test]
    fn the_ladder_steps_down_and_wraps() {
        let three = Some(Duration::from_secs(3));
        assert_eq!(
            step(three),
            Interval::Secs(5),
            "a 3s kind skips nothing below 5s"
        );
        assert_eq!(step(Some(Duration::from_secs(5))), Interval::Secs(10));
        assert_eq!(step(Some(Duration::from_secs(45))), Interval::Secs(60));
        assert_eq!(step(Some(Duration::from_secs(300))), Interval::Secs(3_600));
        assert_eq!(
            step(Some(Duration::from_secs(3_600))),
            Interval::Secs(21_600)
        );
        assert_eq!(
            step(Some(Duration::from_secs(43_200))),
            Interval::Word(Word::Off),
            "past the longest rung there is nothing but off"
        );
        assert_eq!(
            step(None),
            Interval::Word(Word::Auto),
            "off wraps to the curated rhythm"
        );
    }

    /// The global default walks the ladder by position, because it names a rung rather than a
    /// length of time, and wraps from `off` back to `auto`.
    #[test]
    fn the_global_default_steps_by_position_and_wraps() {
        assert_eq!(step_from(None), Interval::Secs(5), "absent means auto");
        assert_eq!(
            step_from(Some(Interval::Word(Word::Auto))),
            Interval::Secs(5)
        );
        assert_eq!(step_from(Some(Interval::Secs(60))), Interval::Secs(300));
        assert_eq!(step_from(Some(Interval::Secs(300))), Interval::Secs(3_600));
        assert_eq!(
            step_from(Some(Interval::Secs(43_200))),
            Interval::Word(Word::Off)
        );
        assert_eq!(
            step_from(Some(Interval::Word(Word::Off))),
            Interval::Word(Word::Auto)
        );
        // A hand-edited number that is not a rung enters at the top rather than nowhere.
        assert_eq!(step_from(Some(Interval::Secs(7))), Interval::Secs(5));
    }

    /// Every rung is drawn through one function, so a value and its completion row cannot be
    /// spelled two ways.
    #[test]
    fn the_ladder_is_spelled_once() {
        assert_eq!(
            LADDER.iter().copied().map(show).collect::<Vec<_>>(),
            [
                "auto", "5s", "10s", "30s", "1m", "5m", "1h", "6h", "12h", "off"
            ]
        );
    }
}
