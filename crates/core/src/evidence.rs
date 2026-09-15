//! From a failed task to what was around it: the alerts raised near its start, and the journal
//! entry if this session launched it. The entity it touched is already the task's own
//! `ENTITIES` section.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nutsh_prism::Entity;

use crate::cell::{Rendered, parse_rfc3339};
use crate::detail::Section;
use crate::journal::Entry;

/// Either side of the task's start.
pub const WINDOW: Duration = Duration::from_secs(10 * 60);

/// Whether this row is a task the pane should gather evidence for.
pub fn wants(entity: &Entity) -> bool {
    entity.kind.id == "prism.config.Task" && entity.raw["status"].as_str() == Some("FAILED")
}

/// `startedTime` ± [`WINDOW`], or `createdTime` for a task that never started.
pub fn window(task: &Entity) -> Option<(SystemTime, SystemTime)> {
    let at = ["startedTime", "createdTime"]
        .iter()
        .find_map(|k| task.raw[k].as_str().and_then(parse_rfc3339))?;
    Some((at - WINDOW, at + WINDOW))
}

/// The `$filter` that asks the Prism Central for the alerts in the window.
pub fn odata(window: (SystemTime, SystemTime)) -> String {
    format!(
        "creationTime ge {} and creationTime le {}",
        rfc3339(window.0),
        rfc3339(window.1)
    )
}

/// Whether an alert was raised inside the window.
pub fn inside(alert: &Entity, window: (SystemTime, SystemTime)) -> bool {
    alert.raw["creationTime"]
        .as_str()
        .and_then(parse_rfc3339)
        .is_some_and(|t| t >= window.0 && t <= window.1)
}

/// The two sections under a failed task's own. `alerts` is `None` until the window has been
/// asked for, and the section says so rather than reading as "none".
pub fn sections(
    task: &Entity,
    alerts: Option<&[&Entity]>,
    journal: Option<&Entry>,
    now: SystemTime,
) -> Vec<Section> {
    let field = |label: &str, text: String, dim: bool| (label.to_string(), Rendered { text, dim });
    let mut out = Vec::new();

    let (title, fields) = match (window(task), alerts) {
        (None, _) => (
            "Alerts".to_string(),
            vec![field("Window", "the task has no start time".into(), true)],
        ),
        (Some(w), None) => (
            format!("Alerts {}", span(w)),
            vec![field("Alerts", "asking…".into(), true)],
        ),
        (Some(w), Some(alerts)) => {
            let mut rows: Vec<&Entity> = alerts.iter().copied().filter(|a| inside(a, w)).collect();
            rows.sort_by_key(|a| a.raw["creationTime"].as_str().map(str::to_string));
            let fields = if rows.is_empty() {
                vec![field("Alerts", "none in the window".into(), true)]
            } else {
                rows.iter()
                    .map(|a| {
                        field(
                            a.raw["severity"].as_str().unwrap_or("-"),
                            format!(
                                "{} · {}",
                                a.raw["title"].as_str().unwrap_or(&a.name),
                                a.raw["creationTime"]
                                    .as_str()
                                    .and_then(parse_rfc3339)
                                    .map_or("-".to_string(), |t| crate::cell::age(t, now))
                            ),
                            false,
                        )
                    })
                    .collect()
            };
            (format!("Alerts {}", span(w)), fields)
        }
    };
    out.push(Section { title, fields });

    out.push(Section {
        title: "Journal".into(),
        fields: match journal {
            Some(e) => vec![
                field("Action", format!("{} on {}", e.action, e.name), false),
                field("Outcome", e.outcome.label().into_owned(), false),
                field("When", crate::cell::age(e.at, now), true),
            ],
            None => vec![field("Started", "not started from here".into(), true)],
        },
    });
    out
}

/// `09:40–10:00 UTC`: the window on the clock, so the title says what was looked at.
fn span(window: (SystemTime, SystemTime)) -> String {
    format!("{}–{} UTC", clock(window.0), clock(window.1))
}

fn clock(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) % 86_400;
    format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(status: &str, started: Option<&str>) -> Entity {
        let mut raw = json!({
            "extId": "ZXJnb24=:t1",
            "operationDescription": "Delete VM",
            "status": status,
            "createdTime": "2026-09-05T09:50:00Z",
            "entitiesAffected": [{"extId": "vm-3", "rel": "vmm:ahv:config:vm", "name": "db-01"}],
        });
        if let Some(s) = started {
            raw["startedTime"] = json!(s);
        }
        Entity::new(nutsh_catalog::kind("prism.config.Task").unwrap(), raw, None)
    }

    fn alert(title: &str, at: &str) -> Entity {
        Entity::new(
            nutsh_catalog::kind("monitoring.serviceability.Alert").unwrap(),
            json!({"extId": title, "title": title, "severity": "WARNING", "creationTime": at}),
            None,
        )
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_788_602_400) // 2026-09-05T10:00:00Z
    }

    /// Only a failed task gathers evidence; a running one is what the footer already follows.
    #[test]
    fn only_a_failed_task_wants_evidence() {
        assert!(wants(&task("FAILED", None)));
        assert!(!wants(&task("SUCCEEDED", None)));
        assert!(!wants(&alert("x", "2026-09-05T09:50:00Z")));
    }

    /// Ten minutes either side of the start, asked for in the API's own spelling, and an
    /// alert one second outside it is outside.
    #[test]
    fn the_window_is_ten_minutes_either_side_of_the_start() {
        let t = task("FAILED", Some("2026-09-05T09:50:01Z"));
        let w = window(&t).unwrap();
        assert_eq!(
            odata(w),
            "creationTime ge 2026-09-05T09:40:01Z and creationTime le 2026-09-05T10:00:01Z"
        );
        assert!(inside(&alert("in", "2026-09-05T09:50:46Z"), w));
        assert!(!inside(&alert("out", "2026-09-05T09:40:00Z"), w));
        // No start: the creation time stands in.
        let never = task("FAILED", None);
        assert!(odata(window(&never).unwrap()).starts_with("creationTime ge 2026-09-05T09:40:00Z"));
    }

    /// The two sections, and what each says when it has nothing.
    #[test]
    fn the_sections_are_the_alerts_in_the_window_and_the_journal() {
        let t = task("FAILED", Some("2026-09-05T09:50:01Z"));
        let inside = alert("Disk space", "2026-09-05T09:50:46Z");
        let outside = alert("CVM unreachable", "2026-09-05T09:40:00Z");
        let s = sections(&t, Some(&[&inside, &outside]), None, now());
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].title, "Alerts 09:40–10:00 UTC");
        assert_eq!(s[0].fields.len(), 1, "the one inside the window");
        assert_eq!(s[0].fields[0].1.text, "Disk space · 9m");
        assert_eq!(s[1].fields[0].1.text, "not started from here");

        let asking = sections(&t, None, None, now());
        assert_eq!(asking[0].fields[0].1.text, "asking…");
        let empty = sections(&t, Some(&[]), None, now());
        assert_eq!(empty[0].fields[0].1.text, "none in the window");
    }
}
