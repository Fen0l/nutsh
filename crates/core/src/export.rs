//! `:export` and `--snapshot --format`: the rows in view, as CSV or JSON.
//!
//! What is exported is what is drawn: the same columns in the same order, the same cell text,
//! names where the table shows names. Nothing here reads a document, a secret or the journal.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Json,
}

impl Format {
    /// `csv` or `json`; anything else is refused with the two words that would have worked.
    pub fn parse(word: &str) -> Result<Format, String> {
        match word.to_ascii_lowercase().as_str() {
            "csv" => Ok(Format::Csv),
            "json" => Ok(Format::Json),
            other => Err(format!("{other} is not a format; csv or json")),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
        }
    }
}

/// A table as text: the headings, and one row of cells per entity, in drawn order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sheet {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Sheet {
    pub fn render(&self, format: Format) -> String {
        match format {
            Format::Csv => self.csv(),
            Format::Json => self.json(),
        }
    }

    /// RFC 4180: a comma, a quote or a line break in a cell quotes the cell, a quote inside is
    /// doubled, records end in `\r\n`.
    fn csv(&self) -> String {
        let mut out = String::new();
        for row in std::iter::once(&self.headers).chain(self.rows.iter()) {
            let line: Vec<String> = row.iter().map(|c| csv_cell(c)).collect();
            out.push_str(&line.join(","));
            out.push_str("\r\n");
        }
        out
    }

    /// One object per row, keyed by heading, values as drawn. Pretty-printed and ended with a
    /// newline, so it reads on a terminal and diffs by line.
    fn json(&self) -> String {
        let rows: Vec<serde_json::Map<String, serde_json::Value>> = self
            .rows
            .iter()
            .map(|row| {
                self.headers
                    .iter()
                    .cloned()
                    .zip(row.iter().map(|c| serde_json::Value::String(c.clone())))
                    .collect()
            })
            .collect();
        let mut out = serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string());
        out.push('\n');
        out
    }
}

fn csv_cell(cell: &str) -> String {
    if cell.contains([',', '"', '\n', '\r']) {
        let mut out = String::with_capacity(cell.len() + 2);
        out.push('"');
        for ch in cell.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
        out
    } else {
        cell.to_string()
    }
}

/// The file an export lands in when nobody named one: `nutsh-<what>-<stamp>.<ext>` in the
/// working directory, stamped to the second so two exports of the same table are two files.
pub fn default_path(what: &str, format: Format, now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamp = stamp(secs);
    let mut name = String::new();
    let _ = write!(name, "nutsh-{what}-{stamp}.{}", format.extension());
    name
}

/// `YYYYMMDD-HHMMSS` in UTC, from the civil-date arithmetic every calendar shares.
fn stamp(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Days since 1970-01-01 to a Gregorian date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Sheet {
        Sheet {
            headers: vec!["NAME".into(), "CLUSTER".into(), "NOTE".into()],
            rows: vec![
                vec!["web-01".into(), "prod-01".into(), "-".into()],
                vec![
                    "db, primary".into(),
                    "He said \"no\"".into(),
                    "two\nlines".into(),
                ],
            ],
        }
    }

    /// A comma, a quote and a line break each quote their cell; a quote inside is doubled.
    #[test]
    fn csv_quotes_what_would_break_a_record_and_nothing_else() {
        let out = sheet().render(Format::Csv);
        assert_eq!(
            out,
            "NAME,CLUSTER,NOTE\r\nweb-01,prod-01,-\r\n\"db, primary\",\"He said \"\"no\"\"\",\"two\nlines\"\r\n"
        );
    }

    /// One object per row, keyed by the heading, every value the text the cell drew.
    #[test]
    fn json_is_one_object_per_row_keyed_by_heading() {
        let out = sheet().render(Format::Json);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v[0]["NAME"], "web-01");
        assert_eq!(v[1]["CLUSTER"], "He said \"no\"");
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert!(out.ends_with('\n'));
    }

    /// An empty table is a header line, or an empty list - never nothing at all.
    #[test]
    fn an_empty_sheet_still_says_its_columns() {
        let s = Sheet {
            headers: vec!["NAME".into()],
            rows: vec![],
        };
        assert_eq!(s.render(Format::Csv), "NAME\r\n");
        assert_eq!(s.render(Format::Json), "[]\n");
    }

    #[test]
    fn the_format_word_is_csv_or_json() {
        assert_eq!(Format::parse("CSV"), Ok(Format::Csv));
        assert_eq!(Format::parse("json"), Ok(Format::Json));
        assert!(Format::parse("xlsx").unwrap_err().contains("csv or json"));
    }

    /// The stamp is the UTC second, so two exports a second apart are two files.
    #[test]
    fn the_default_path_is_stamped_to_the_second() {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_788_602_400);
        assert_eq!(
            default_path("vm", Format::Csv, at),
            "nutsh-vm-20260905-100000.csv"
        );
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(951_782_400), "20000229-000000", "a leap day");
    }
}
