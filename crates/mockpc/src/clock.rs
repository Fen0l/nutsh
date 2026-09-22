//! RFC 3339 on the wire: whole seconds and `Z`, the shape every fixture timestamp has.

use std::time::{SystemTime, UNIX_EPOCH};

/// `t` as `2026-09-05T10:00:00Z`. Whole seconds on purpose: two stamps written in the same
/// second are then equal rather than ordered by digits nobody chose, and the string orders
/// bytewise the way the instant does. The same civil-from-days arithmetic `nutsh-core` uses.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = i64::try_from(days).unwrap_or(i64::MAX / 4) + 719_468;
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
    use std::time::Duration;

    #[test]
    fn formats_the_fixtures_reference_instant() {
        // 2026-09-05T10:00:00Z, the instant every bundled fixture is written around.
        let t0 = UNIX_EPOCH + Duration::from_secs(1_788_602_400);
        assert_eq!(rfc3339(t0), "2026-09-05T10:00:00Z");
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(t0 + Duration::from_millis(999)),
            "2026-09-05T10:00:00Z",
            "whole seconds"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_772_236_800)),
            "2026-02-28T00:00:00Z"
        );
    }
}
