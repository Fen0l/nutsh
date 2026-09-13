//! JSON to YAML text for the detail view. Keys keep their JSON order.

use serde_json::Value;

/// `v` as YAML: two-space indentation, `key: value`, `- item`, strings quoted only when YAML
/// would read them as something else.
pub fn render(v: &Value) -> String {
    let mut out = String::new();
    write(v, 0, &mut out);
    out
}

fn write(v: &Value, indent: usize, out: &mut String) {
    match v {
        Value::Object(map) if map.is_empty() => push_line(out, indent, "{}"),
        Value::Array(items) if items.is_empty() => push_line(out, indent, "[]"),
        Value::Object(map) => {
            for (k, child) in map {
                match child {
                    Value::Object(m) if !m.is_empty() => {
                        push_line(out, indent, &format!("{k}:"));
                        write(child, indent + 1, out);
                    }
                    Value::Array(a) if !a.is_empty() => {
                        push_line(out, indent, &format!("{k}:"));
                        write(child, indent + 1, out);
                    }
                    _ => push_line(out, indent, &format!("{k}: {}", scalar(child))),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(m) if !m.is_empty() => {
                        // `- key: value` for the first pair, then the rest indented under it.
                        let mut first = true;
                        for (k, child) in m {
                            let prefix = if first { "- " } else { "  " };
                            first = false;
                            match child {
                                Value::Object(mm) if !mm.is_empty() => {
                                    push_line(out, indent, &format!("{prefix}{k}:"));
                                    write(child, indent + 2, out);
                                }
                                Value::Array(aa) if !aa.is_empty() => {
                                    push_line(out, indent, &format!("{prefix}{k}:"));
                                    write(child, indent + 2, out);
                                }
                                _ => push_line(
                                    out,
                                    indent,
                                    &format!("{prefix}{k}: {}", scalar(child)),
                                ),
                            }
                        }
                    }
                    Value::Array(a) if !a.is_empty() => {
                        push_line(out, indent, "-");
                        write(item, indent + 1, out);
                    }
                    _ => push_line(out, indent, &format!("- {}", scalar(item))),
                }
            }
        }
        scalar_value => push_line(out, indent, &scalar(scalar_value)),
    }
}

fn push_line(out: &mut String, indent: usize, text: &str) {
    for _ in 0..indent {
        out.push_str("  ");
    }
    out.push_str(text);
    out.push('\n');
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => quote_if_needed(s),
        Value::Object(_) => "{}".to_string(),
        Value::Array(_) => "[]".to_string(),
        other => other.to_string(),
    }
}

fn quote_if_needed(s: &str) -> String {
    let looks_like_other_type = s.is_empty()
        || matches!(
            s.to_ascii_lowercase().as_str(),
            "true"
                | "false"
                | "null"
                | "~"
                | "yes"
                | "no"
                | "on"
                | "off"
                | ".inf"
                | "-.inf"
                | "+.inf"
                | ".nan"
        )
        || s.parse::<f64>().is_ok()
        || s.starts_with("0x")
        || s.starts_with("0o")
        || s.starts_with("0b")
        || s != s.trim();
    let starts_badly = s.starts_with([
        '-', '*', '&', '!', '%', '@', '`', '[', ']', '{', '}', '|', '>', '\'', '"', ' ',
    ]);
    let contains_badly = s.contains(": ")
        || s.contains(" #")
        || s.starts_with('#')
        || s.chars().any(char::is_control)
        || s.contains('"')
        || s.ends_with(':')
        || s.contains(":\t");
    if looks_like_other_type || starts_badly || contains_badly {
        let escaped = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_objects_and_arrays() {
        let v = json!({
            "name": "web-01",
            "cluster": {"extId": "c1"},
            "disks": [{"index": 0, "sizeBytes": 10}, {"index": 1}],
            "tags": ["a", "b"],
            "empty": [],
            "none": {}
        });
        let want = "\
name: web-01
cluster:
  extId: c1
disks:
  - index: 0
    sizeBytes: 10
  - index: 1
tags:
  - a
  - b
empty: []
none: {}
";
        assert_eq!(render(&v), want);
    }

    #[test]
    fn strings_are_quoted_only_when_yaml_would_misread_them() {
        let cases = [
            ("plain", "plain"),
            ("", "\"\""),
            ("true", "\"true\""),
            ("42", "\"42\""),
            ("1.5", "\"1.5\""),
            ("- dash", "\"- dash\""),
            ("*star", "\"*star\""),
            ("&amp", "\"&amp\""),
            ("key: value", "\"key: value\""),
            ("a #b", "\"a #b\""),
            ("say \"hi\"", "\"say \\\"hi\\\"\""),
            ("multi\nline", "\"multi\\nline\""),
            ("2026-09-05T10:00:00Z", "2026-09-05T10:00:00Z"),
            ("see below:", "\"see below:\""),
            ("Null", "\"Null\""),
            ("ON", "\"ON\""),
            ("0x1F", "\"0x1F\""),
            ("10.0.0.1", "10.0.0.1"),
        ];
        for (input, want) in cases {
            assert_eq!(render(&json!(input)), format!("{want}\n"), "{input:?}");
        }
    }

    #[test]
    fn scalars_at_top_level() {
        assert_eq!(render(&json!(7)), "7\n");
        assert_eq!(render(&json!(true)), "true\n");
        assert_eq!(render(&json!(null)), "null\n");
    }

    fn json_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                json_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                out.push(path);
            }
        }
    }

    #[test]
    fn renders_yaml_that_parses_back_to_the_same_value() {
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../mockpc/fixtures");
        let mut files = Vec::new();
        json_files(std::path::Path::new(fixtures), &mut files);
        assert!(!files.is_empty(), "no fixtures under {fixtures}");
        for file in &files {
            let text =
                std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            let v: Value =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            let entities: Vec<Value> = v.as_array().cloned().unwrap_or_else(|| vec![v]);
            for entity in &entities {
                let rendered = render(entity);
                let parsed: Value = serde_yaml_ng::from_str(&rendered)
                    .unwrap_or_else(|e| panic!("{}: {e}\n{rendered}", file.display()));
                assert_eq!(&parsed, entity, "{}", file.display());
            }
        }

        // `serde_yaml_ng` follows the YAML 1.2 core schema, under which `ON`/`OFF` are plain
        // strings, not booleans - so they never round-trip incorrectly here. The explicit
        // quoting cases above are what actually guard against a YAML 1.1 reader.
        let adversarial = json!({
            "a": "see below:",
            "b": "Null",
            "c": "0x1F",
            "d": ["- x", "*y", "k: v", "", "true", "1e5", "a\rb", "tab\there", "trail ", " lead", ".inf", "0b101", "-.inf"],
            "e": {"f": [[1, 2], [3]], "g": [{"h": {"i": [{"j": "deep"}]}}]},
            "t": "2026-09-05T10:00:00Z",
            "ip": "10.0.0.1",
        });
        let rendered = render(&adversarial);
        let parsed: Value =
            serde_yaml_ng::from_str(&rendered).unwrap_or_else(|e| panic!("{e}\n{rendered}"));
        assert_eq!(parsed, adversarial);
    }
}
