//! Reading logical fields out of an OTLP/JSON payload.
//!
//! A check names the fields that must survive the round trip (`body`,
//! `severityText`, …). Those names are the OTLP ones, not any backend's column
//! names, so the same check can be pointed at any backend and the adapter is
//! responsible for saying where it keeps each one.

use serde_json::Value;

/// Extracts a logical field from an OTLP/JSON logs payload.
///
/// Returns `None` when the field is absent, which is a meaningful answer: a
/// check comparing an absent field against an absent field passes.
pub fn logical_field(payload: &Value, field: &str) -> Option<Value> {
    let record = first_log_record(payload)?;
    match field {
        // `body` is an AnyValue. Compare the value it carries rather than the
        // wrapper, since backends store the value and not the OTLP envelope.
        "body" => any_value(record.get("body")?),
        "severityText" | "severity_text" => record.get("severityText").cloned(),
        "severityNumber" | "severity_number" => record.get("severityNumber").cloned(),
        "timeUnixNano" | "time_unix_nano" => record.get("timeUnixNano").cloned(),
        "observedTimeUnixNano" => record.get("observedTimeUnixNano").cloned(),
        "traceId" => record.get("traceId").cloned(),
        "spanId" => record.get("spanId").cloned(),
        // `attributes.foo` reads a log-record attribute by key.
        other => match other.strip_prefix("attributes.") {
            Some(key) => attribute(record.get("attributes")?, key),
            None => record.get(other).cloned(),
        },
    }
}

fn first_log_record(payload: &Value) -> Option<&Value> {
    payload
        .get("resourceLogs")?
        .as_array()?
        .first()?
        .get("scopeLogs")?
        .as_array()?
        .first()?
        .get("logRecords")?
        .as_array()?
        .first()
}

/// Unwraps an OTLP `AnyValue` to the value inside it.
fn any_value(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    for key in [
        "stringValue",
        "boolValue",
        "intValue",
        "doubleValue",
        "bytesValue",
        "arrayValue",
        "kvlistValue",
    ] {
        if let Some(inner) = object.get(key) {
            return Some(inner.clone());
        }
    }
    // An AnyValue with nothing set is a legitimate empty value, not an error.
    Some(Value::Null)
}

fn attribute(attributes: &Value, key: &str) -> Option<Value> {
    attributes
        .as_array()?
        .iter()
        .find(|kv| kv.get("key").and_then(|k| k.as_str()) == Some(key))
        .and_then(|kv| kv.get("value"))
        .and_then(any_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        serde_json::json!({
            "resourceLogs": [{
                "resource": {"attributes": []},
                "scopeLogs": [{
                    "scope": {"name": "specmatrix"},
                    "logRecords": [{
                        "timeUnixNano": "1755000000000000000",
                        "severityText": "INFO",
                        "body": {"stringValue": "hello"},
                        "attributes": [
                            {"key": "specmatrix.run", "value": {"stringValue": "sm-1234"}}
                        ]
                    }]
                }]
            }]
        })
    }

    #[test]
    fn reads_body_through_the_any_value_wrapper() {
        assert_eq!(logical_field(&payload(), "body"), Some(Value::from("hello")));
    }

    #[test]
    fn reads_an_attribute_by_key() {
        assert_eq!(
            logical_field(&payload(), "attributes.specmatrix.run"),
            Some(Value::from("sm-1234"))
        );
    }

    /// An absent field is an answer, not a failure: a check comparing absent
    /// against absent should pass rather than error.
    #[test]
    fn absent_field_reads_as_none() {
        assert_eq!(logical_field(&payload(), "traceId"), None);
    }

    #[test]
    fn empty_payload_yields_nothing() {
        assert_eq!(logical_field(&serde_json::json!({}), "body"), None);
    }
}
