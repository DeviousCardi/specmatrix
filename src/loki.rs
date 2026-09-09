//! Reading logical fields out of a Loki push-API body.
//!
//! The mirror of `src/otlp.rs` and `src/es.rs`: a check names fields in the
//! protocol's own terms — `line`, `timestamp`, `labels`, a structured-metadata
//! key — and this reads them from the JSON a case actually sent, so they can
//! be compared with what a store returned.
//!
//! There is no specification for this API. Loki defines it by what it does,
//! which is why it is the reference column: a case that cites `basis:
//! de-facto` here is citing Loki's own behaviour, confirmed against a running
//! container, not a document.

use serde_json::Value;

/// Reads a field from the first value of the first stream in a push body.
///
/// A corpus case sends one stream with one value unless the check is
/// specifically about several of either, so "first" is "the one that matters"
/// for every case this project has written so far. A check that needs another
/// would need a `series`-style selector, same as remote-write; none has yet.
pub fn logical_field(sent: &Value, field: &str) -> Option<Value> {
    let stream = sent.get("streams")?.as_array()?.first()?;
    match field {
        "labels" => stream.get("stream").cloned(),
        other => {
            let value = stream.get("values")?.as_array()?.first()?;
            match other {
                "timestamp" => value.get(0).cloned(),
                "line" => value.get(1).cloned(),
                // A structured-metadata key, read from the value's optional
                // third element. `metadata.trace_id` mirrors the
                // `attributes.foo` convention in `src/otlp.rs`.
                other => match other.strip_prefix("metadata.") {
                    Some(key) => value.get(2)?.get(key).cloned(),
                    None => None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn push_body() -> Value {
        json!({
            "streams": [{
                "stream": {"service_name": "specmatrix", "specmatrix_run": "sm-1"},
                "values": [["1757000000000000000", "hello", {"trace_id": "abc123"}]]
            }]
        })
    }

    #[test]
    fn reads_the_line() {
        assert_eq!(logical_field(&push_body(), "line"), Some(json!("hello")));
    }

    #[test]
    fn reads_the_timestamp() {
        assert_eq!(logical_field(&push_body(), "timestamp"), Some(json!("1757000000000000000")));
    }

    #[test]
    fn reads_the_stream_labels_as_an_object() {
        assert_eq!(
            logical_field(&push_body(), "labels"),
            Some(json!({"service_name": "specmatrix", "specmatrix_run": "sm-1"}))
        );
    }

    #[test]
    fn reads_a_structured_metadata_key() {
        assert_eq!(logical_field(&push_body(), "metadata.trace_id"), Some(json!("abc123")));
    }

    /// A value with no third element has no structured metadata, and asking
    /// for one is absent rather than an error — the shape most cases use.
    #[test]
    fn a_value_without_metadata_reads_metadata_keys_as_absent() {
        let body = json!({"streams": [{"stream": {}, "values": [["1", "line only"]]}]});
        assert_eq!(logical_field(&body, "metadata.trace_id"), None);
    }
}
