//! The v4 response envelope: `{ "metadata": {...}, "data": <entity | [entities] | error> }`.

use serde::Deserialize;
use serde_json::Value;

use crate::error::PrismError;

#[derive(Debug, Default, Deserialize)]
pub struct Envelope {
    pub metadata: Option<Metadata>,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub total_available_results: Option<u64>,
}

pub fn parse(body: &[u8]) -> Result<Envelope, PrismError> {
    serde_json::from_slice(body).map_err(|e| PrismError::Decode(e.to_string()))
}

/// Human message from an error body, or `None` when it carries none. A v4 envelope's
/// `data.error` is either a list of `AppMessage { message, .. }` or a `SchemaValidationError`
/// `{ error, validationErrorMessages: [{ message, attributePath, .. }] }`. The API gateway's
/// own errors are bare `{ "message": "Connection was refused" }` (a service that is not
/// deployed, for example Reports without Intelligent Operations), and are read too.
pub fn error_message(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    match value.get("data").and_then(|d| d.get("error")) {
        Some(Value::Array(items)) => join_messages(items),
        Some(Value::Object(o)) => o
            .get("validationErrorMessages")
            .and_then(Value::as_array)
            .and_then(|items| join_messages(items))
            .or_else(|| o.get("error").and_then(Value::as_str).map(str::to_string))
            .or_else(|| o.get("message").and_then(Value::as_str).map(str::to_string)),
        _ => value
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn join_messages(items: &[Value]) -> Option<String> {
    let msgs: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("message").and_then(Value::as_str))
        .collect();
    if msgs.is_empty() {
        None
    } else {
        Some(msgs.join("; "))
    }
}

pub fn list_items(env: Envelope) -> Result<(Vec<Value>, Option<u64>), PrismError> {
    let total = env.metadata.and_then(|m| m.total_available_results);
    match env.data {
        Value::Array(items) => Ok((items, total)),
        Value::Null => Ok((Vec::new(), total)),
        other => Err(PrismError::Decode(format!(
            "expected a list, got {}",
            type_name(&other)
        ))),
    }
}

pub fn one(env: Envelope) -> Result<Value, PrismError> {
    if env.data.is_object() {
        Ok(env.data)
    } else {
        Err(PrismError::Decode(format!(
            "expected an object, got {}",
            type_name(&env.data)
        )))
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_joins_app_messages() {
        let body = br#"{"data":{"error":[{"message":"VM is powered off","code":"VMM-1"},{"message":"try again"}]}}"#;
        assert_eq!(
            error_message(body).as_deref(),
            Some("VM is powered off; try again")
        );
        assert_eq!(error_message(b"not json").as_deref(), None);
        assert_eq!(
            error_message(br#"{"data":{"error":{"message":"single"}}}"#).as_deref(),
            Some("single")
        );
        let validation = br#"{"data":{"error":{"error":"Validation failed","validationErrorMessages":[{"message":"name is required","attributePath":"$.name"},{"message":"memorySizeBytes must be positive"}]}}}"#;
        assert_eq!(
            error_message(validation).as_deref(),
            Some("name is required; memorySizeBytes must be positive")
        );
        assert_eq!(
            error_message(br#"{"data":{"error":{"error":"Validation failed"}}}"#).as_deref(),
            Some("Validation failed")
        );
        // The gateway's own error, seen on a PC whose reporting service is not deployed.
        assert_eq!(
            error_message(br#"{"message": "Connection was refused"}"#).as_deref(),
            Some("Connection was refused")
        );
        assert_eq!(error_message(br#"{"data": {"x": 1}}"#), None);
    }

    #[test]
    fn list_items_accepts_null_data() {
        let env = parse(br#"{"metadata":{"totalAvailableResults":0},"data":null}"#).unwrap();
        let (items, total) = list_items(env).unwrap();
        assert!(items.is_empty());
        assert_eq!(total, Some(0));
    }
}
