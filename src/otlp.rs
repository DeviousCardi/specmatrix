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

/// Reads a field out of an OTLP metrics payload.
///
/// The first data point of the named metric, or of the only metric when a check
/// names none. Naming matters for the same reason it does in remote-write: one
/// export carries several metrics, and a check that did not say which would be
/// asserting on whichever happened to be serialised first.
///
/// The point's value is read whatever shape it came in — a gauge or sum carries
/// `asDouble` or `asInt`, a histogram carries `count`, `sum`, `bucketCounts`
/// and `explicitBounds` — so a check writes `on: [value]` and does not have to
/// know which of them the payload used.
pub fn metric_field(payload: &Value, field: &str, metric: Option<&str>) -> Option<Value> {
    let chosen = find_metric(payload, metric)?;
    match field {
        "name" => chosen.get("name").cloned(),
        "unit" => chosen.get("unit").cloned(),
        "value" => point_value(first_point(chosen)?),
        "count" => first_point(chosen)?.get("count").cloned(),
        "sum" => first_point(chosen)?.get("sum").cloned(),
        "bucketCounts" => first_point(chosen)?.get("bucketCounts").cloned(),
        "explicitBounds" => first_point(chosen)?.get("explicitBounds").cloned(),
        "timeUnixNano" => first_point(chosen)?.get("timeUnixNano").cloned(),
        "labels" => {
            // The point's attributes as a flat map, so they compare against the
            // label set a Prometheus-shaped read-back returns.
            let mut map = serde_json::Map::new();
            for kv in first_point(chosen)?.get("attributes")?.as_array()? {
                let (Some(key), Some(value)) =
                    (kv.get("key")?.as_str(), kv.get("value").and_then(any_value))
                else {
                    continue;
                };
                map.insert(key.to_string(), value);
            }
            Some(Value::Object(map))
        }
        other => match other.strip_prefix("attributes.") {
            Some(key) => attribute(first_point(chosen)?.get("attributes")?, key),
            None => chosen.get(other).cloned(),
        },
    }
}

fn find_metric<'a>(payload: &'a Value, name: Option<&str>) -> Option<&'a Value> {
    let metrics: Vec<&Value> = payload
        .get("resourceMetrics")?
        .as_array()?
        .iter()
        .filter_map(|r| r.get("scopeMetrics")?.as_array())
        .flatten()
        .filter_map(|s| s.get("metrics")?.as_array())
        .flatten()
        .collect();
    match name {
        Some(name) => {
            metrics.into_iter().find(|m| m.get("name").and_then(Value::as_str) == Some(name))
        }
        None if metrics.len() == 1 => metrics.into_iter().next(),
        // Under-specified rather than "the first one", for the same reason as
        // remote-write: guessing gives a check a verdict about a metric it
        // never named.
        None => None,
    }
}

/// The first data point, whichever of the five point-carrying shapes the metric
/// used. A metric carries exactly one of them.
fn first_point(metric: &Value) -> Option<&Value> {
    for kind in ["gauge", "sum", "histogram", "exponentialHistogram", "summary"] {
        if let Some(points) = metric.get(kind).and_then(|k| k.get("dataPoints")) {
            return points.as_array()?.first();
        }
    }
    None
}

/// A point's value, from whichever field carries it.
///
/// `asInt` is a JSON string in OTLP, because the field is an int64 and JSON
/// numbers cannot hold one exactly. It is returned as written; the check's
/// `as: integer` or `as: float` decides how it is compared, and a reader that
/// coerced here would hide a store that lost the precision.
fn point_value(point: &Value) -> Option<Value> {
    for field in ["asDouble", "asInt", "count"] {
        if let Some(value) = point.get(field) {
            return Some(value.clone());
        }
    }
    None
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

// ---------------------------------------------------------------------------
// What a store said when it took the write
// ---------------------------------------------------------------------------

/// The report a store returned alongside a 2xx on an OTLP export.
///
/// OTLP does not make a successful status mean the data was kept. A server that
/// declines part of a batch says so in
/// `ExportLogsServiceResponse.partial_success`, which carries the number of
/// records it dropped and a message. A runner that classifies on the HTTP
/// status alone cannot tell a store that reported a discard from one that said
/// nothing, and for this project that is the whole distinction: the first
/// failure is loud, the second is silent.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportReport {
    pub rejected: i64,
    pub message: String,
    /// Set when the response body is not in the encoding the request used. The
    /// specification requires the response to match the request's encoding, so
    /// a JSON export answered with protobuf carries a report no conformant
    /// JSON client can read — the discard is announced in a form nobody parses.
    pub encoding_mismatch: Option<String>,
}

/// Reads a store's export response, whatever encoding it actually used.
///
/// Returns `None` when the body carries no report at all, which is the ordinary
/// case for a store that kept everything.
pub fn export_report(
    request_encoding: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Option<ExportReport> {
    if body.is_empty() {
        return None;
    }
    let expected_json = request_encoding == "otlp-json";

    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        let partial = value.get("partialSuccess").or_else(|| value.get("partial_success"))?;
        let rejected = partial
            .get("rejectedLogRecords")
            .or_else(|| partial.get("rejected_log_records"))
            .and_then(number_from)
            .unwrap_or(0);
        let message = partial
            .get("errorMessage")
            .or_else(|| partial.get("error_message"))
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        let mismatch =
            (!expected_json).then(|| "request was protobuf, response body is JSON".to_string());
        return Some(ExportReport { rejected, message, encoding_mismatch: mismatch });
    }

    // Not JSON. It may still be a perfectly good report, in the wrong encoding.
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceResponse;
    use prost::Message;
    let decoded = ExportLogsServiceResponse::decode(body).ok()?;
    let partial = decoded.partial_success?;
    let mismatch = expected_json.then(|| {
        let claimed = content_type.unwrap_or("none");
        format!("request was JSON, response body is protobuf and content-type says {claimed}")
    });
    Some(ExportReport {
        rejected: partial.rejected_log_records,
        message: partial.error_message,
        encoding_mismatch: mismatch,
    })
}

fn number_from(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        // proto3 JSON encodes int64 as a string.
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
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

    fn protobuf_response(rejected: i64, message: &str) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::logs::v1::{
            ExportLogsPartialSuccess, ExportLogsServiceResponse,
        };
        use prost::Message;
        ExportLogsServiceResponse {
            partial_success: Some(ExportLogsPartialSuccess {
                rejected_log_records: rejected,
                error_message: message.to_string(),
            }),
        }
        .encode_to_vec()
    }

    /// A store that kept everything says nothing, and that is not a report.
    #[test]
    fn an_empty_body_carries_no_report() {
        assert_eq!(export_report("otlp-json", Some("application/json"), b""), None);
    }

    #[test]
    fn a_success_with_no_partial_success_carries_no_report() {
        assert_eq!(export_report("otlp-json", Some("application/json"), b"{}"), None);
    }

    #[test]
    fn a_json_report_is_read_in_either_field_naming() {
        let camel = br#"{"partialSuccess":{"rejectedLogRecords":"3","errorMessage":"too old"}}"#;
        let report = export_report("otlp-json", Some("application/json"), camel).unwrap();
        assert_eq!(report.rejected, 3);
        assert_eq!(report.message, "too old");
        assert_eq!(report.encoding_mismatch, None);

        let snake = br#"{"partial_success":{"rejected_log_records":3,"error_message":"too old"}}"#;
        assert_eq!(
            export_report("otlp-json", Some("application/json"), snake).unwrap().rejected,
            3
        );
    }

    #[test]
    fn a_protobuf_report_to_a_protobuf_request_is_not_a_mismatch() {
        let body = protobuf_response(1, "too old");
        let report = export_report("otlp-protobuf", Some("application/x-protobuf"), &body).unwrap();
        assert_eq!(report.rejected, 1);
        assert_eq!(report.message, "too old");
        assert_eq!(report.encoding_mismatch, None);
    }

    /// OpenObserve v0.92.2 does exactly this: a JSON export is answered with a
    /// protobuf body under `content-type: application/json`. The discard is
    /// reported through the channel OTLP defines for it, in a form no
    /// conformant JSON client can read.
    #[test]
    fn a_protobuf_report_to_a_json_request_is_reported_as_a_mismatch() {
        let body = protobuf_response(1, "Too old data, only last 5 hours data can be ingested.");
        let report = export_report("otlp-json", Some("application/json"), &body).unwrap();
        assert_eq!(report.rejected, 1);
        assert!(report.message.starts_with("Too old data"));
        let mismatch = report.encoding_mismatch.expect("a mismatch");
        assert!(mismatch.contains("response body is protobuf"), "{mismatch}");
        assert!(mismatch.contains("application/json"), "{mismatch}");
    }

    /// A body that is neither is not a report, and must not be guessed at.
    #[test]
    fn an_unreadable_body_carries_no_report() {
        assert_eq!(export_report("otlp-json", None, b"not a response at all"), None);
    }
}
