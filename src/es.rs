//! Reading logical fields out of an Elasticsearch `_bulk` body.
//!
//! The mirror of `src/otlp.rs`: a check names fields in the protocol's own
//! terms and the adapter says where the backend keeps each one.
//!
//! A `_bulk` body is NDJSON, alternating an action line with a source line, and
//! only the source lines carry content. Getting that pairing wrong reads a
//! bulk action as a document, so a check naming `index` would silently compare
//! against metadata.

use anyhow::{Context, Result};
use serde_json::Value;

/// Parses an NDJSON `_bulk` body into the documents it carries, discarding the
/// action lines.
pub fn parse_bulk(payload: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(payload).context("a _bulk body must be valid UTF-8")?;
    let mut documents = Vec::new();
    let mut expecting_source = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let value: Value =
            serde_json::from_str(line).with_context(|| format!("parsing _bulk line {line:?}"))?;
        if expecting_source {
            documents.push(value);
            expecting_source = false;
        } else {
            // `delete` carries no source line; every other action does.
            expecting_source = value.get("delete").is_none();
        }
    }
    Ok(Value::Array(documents))
}

pub fn documents(parsed: &Value) -> Vec<&Value> {
    parsed.as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

/// Reads a field from the first document in the body, matching the shape of
/// `otlp::logical_field`.
pub fn logical_field(parsed: &Value, field: &str) -> Option<Value> {
    documents(parsed).first()?.get(field).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bulk() -> &'static str {
        concat!(
            r#"{"index":{"_id":"a"}}"#,
            "\n",
            r#"{"message":"hello","level":"INFO","specmatrix.run":"sm-1"}"#,
            "\n",
            r#"{"index":{"_id":"b"}}"#,
            "\n",
            r#"{"message":"second","specmatrix.run":"sm-1"}"#,
            "\n",
        )
    }

    #[test]
    fn reads_a_field_from_the_first_source_line() {
        let sent = parse_bulk(bulk().as_bytes()).expect("parses");
        assert_eq!(logical_field(&sent, "message"), Some(json!("hello")));
        assert_eq!(logical_field(&sent, "level"), Some(json!("INFO")));
    }

    /// An absent field is an answer, not a failure: a check comparing absent
    /// against absent passes.
    #[test]
    fn an_absent_field_reads_as_none() {
        let sent = parse_bulk(bulk().as_bytes()).expect("parses");
        assert_eq!(logical_field(&sent, "trace_id"), None);
    }

    /// Action lines are metadata, not content. A check naming `index` must not
    /// read the bulk action instead of a document.
    #[test]
    fn action_lines_are_not_treated_as_documents() {
        let sent = parse_bulk(bulk().as_bytes()).expect("parses");
        assert_eq!(logical_field(&sent, "index"), None);
    }

    #[test]
    fn every_document_is_available_by_position() {
        let sent = parse_bulk(bulk().as_bytes()).expect("parses");
        assert_eq!(documents(&sent).len(), 2);
        assert_eq!(documents(&sent)[1].get("message").unwrap(), "second");
    }

    /// The API requires a trailing newline, which must not produce an empty
    /// document.
    #[test]
    fn a_trailing_newline_does_not_add_a_document() {
        let sent = parse_bulk(b"{\"index\":{}}\n{\"a\":1}\n").expect("parses");
        assert_eq!(documents(&sent).len(), 1);
    }

    /// A delete action carries no source line, so the line after it is the next
    /// action rather than a document.
    #[test]
    fn a_delete_action_consumes_no_source_line() {
        let body = concat!(
            r#"{"delete":{"_id":"gone"}}"#,
            "\n",
            r#"{"index":{"_id":"kept"}}"#,
            "\n",
            r#"{"message":"still here"}"#,
            "\n",
        );
        let sent = parse_bulk(body.as_bytes()).expect("parses");
        assert_eq!(documents(&sent).len(), 1);
        assert_eq!(logical_field(&sent, "message"), Some(json!("still here")));
    }

    #[test]
    fn a_malformed_line_is_an_error_rather_than_a_skipped_document() {
        assert!(parse_bulk(b"{\"index\":{}}\nnot json\n").is_err());
    }
}
