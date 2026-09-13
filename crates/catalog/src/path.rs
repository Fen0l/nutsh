//! Dotted-path access into entity JSON: `cluster.extId`, `nics[].networkInfo.ipv4Config.ipAddress.value`.

use serde_json::Value;

/// Every value at `path`. A segment `name[]` descends into each element of the array at
/// `name`; a missing segment contributes nothing. Never panics.
pub fn get<'a>(v: &'a Value, path: &str) -> Vec<&'a Value> {
    if path.is_empty() {
        return vec![v];
    }
    let mut current = vec![v];
    for segment in path.split('.') {
        let (key, fan_out) = match segment.strip_suffix("[]") {
            Some(k) => (k, true),
            None => (segment, false),
        };
        let mut next = Vec::new();
        for value in current {
            let Some(child) = value.get(key) else {
                continue;
            };
            if fan_out {
                if let Some(items) = child.as_array() {
                    next.extend(items.iter());
                }
            } else {
                next.push(child);
            }
        }
        current = next;
    }
    current
}

/// The first value at `path`, if any.
pub fn first<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    get(v, path).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vm() -> Value {
        json!({
            "name": "web-01",
            "cluster": {"extId": "c1"},
            "nics": [
                {"networkInfo": {"ipv4Config": {"ipAddress": {"value": "10.0.0.1"}}}},
                {"networkInfo": {"ipv4Config": {"ipAddress": {"value": "10.0.0.2"}}}},
                {"networkInfo": {}}
            ],
            "disks": []
        })
    }

    #[test]
    fn scalar_and_nested_paths() {
        let v = vm();
        assert_eq!(first(&v, "name"), Some(&json!("web-01")));
        assert_eq!(first(&v, "cluster.extId"), Some(&json!("c1")));
        assert_eq!(first(&v, "cluster.name"), None);
        assert_eq!(first(&v, "nope.deeper"), None);
    }

    #[test]
    fn array_segments_fan_out_and_skip_misses() {
        let v = vm();
        let ips: Vec<&str> = get(&v, "nics[].networkInfo.ipv4Config.ipAddress.value")
            .into_iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(ips, ["10.0.0.1", "10.0.0.2"]);
        assert!(get(&v, "disks[].x").is_empty());
        assert_eq!(get(&v, "nics").len(), 1, "an array without [] is one value");
        assert_eq!(get(&v, "nics[]").len(), 3);
    }

    #[test]
    fn empty_path_is_the_value_itself() {
        let v = json!(7);
        assert_eq!(first(&v, ""), Some(&v));
    }
}
