//! Turning a case payload into the bytes a backend will accept.
//!
//! The corpus keeps every payload in a native, hand-sendable format — real OTLP
//! JSON, real NDJSON — so a maintainer can send the file at their own backend
//! with curl and confirm a finding without installing anything. A backend that
//! speaks a different encoding of the same protocol is not failing the check,
//! so the runner converts rather than reporting an ineligibility that is really
//! ours.
//!
//! 0.1 got this wrong in an instructive way. Quickwit accepts OTLP protobuf and
//! refuses JSON at the Content-Type header, and the suite reported five
//! identical rejections — which looks like five findings and is one fact. The
//! `N/A` verdict fixed the reporting; this module fixes the cause.

use anyhow::{Context, Result};

/// Converts `payload`, written in `native`, into `encoding`.
pub fn to_wire(native: &str, encoding: &str, payload: &[u8]) -> Result<Vec<u8>> {
    if native == encoding {
        return Ok(payload.to_vec());
    }
    match (native, encoding) {
        ("otlp-json", "otlp-protobuf") => otlp_logs_json_to_protobuf(payload),
        _ => anyhow::bail!("no encoder from {native} to {encoding}"),
    }
}

fn otlp_logs_json_to_protobuf(payload: &[u8]) -> Result<Vec<u8>> {
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use prost::Message;

    // A strict parse, deliberately. A payload that is not valid UTF-8 cannot
    // become a protobuf string field, and a lossy decode here would quietly
    // replace the very bytes the invalid-UTF-8 case exists to send — turning a
    // check about the backend into a check about this function.
    let request: ExportLogsServiceRequest = serde_json::from_slice(payload)
        .context("decoding OTLP JSON into ExportLogsServiceRequest")?;
    Ok(request.encode_to_vec())
}

/// The first encoding a case offers that the backend accepts.
///
/// An adapter with no `formats:` has not declared any and is treated as
/// accepting everything, so adapters written before this existed keep working.
pub fn choose_encoding(accepted: &[String], offered: &[String]) -> Option<String> {
    if accepted.is_empty() {
        return offered.first().cloned();
    }
    offered.iter().find(|e| accepted.contains(e)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{"resourceLogs":[{"resource":{"attributes":[
        {"key":"service.name","value":{"stringValue":"specmatrix"}}]},
        "scopeLogs":[{"scope":{"name":"specmatrix"},"logRecords":[
        {"timeUnixNano":"1755000000123456789","severityNumber":9,
         "severityText":"INFO","body":{"stringValue":"hello"}}]}]}]}"#;

    /// A payload the corpus keeps in its native format must reach the backend
    /// unchanged, or a maintainer cannot reproduce a finding with curl and the
    /// file.
    #[test]
    fn a_matching_encoding_is_passed_through_untouched() {
        let out = to_wire("otlp-json", "otlp-json", MINIMAL.as_bytes()).unwrap();
        assert_eq!(out, MINIMAL.as_bytes());
    }

    #[test]
    fn otlp_json_becomes_protobuf_that_decodes_back_to_the_same_record() {
        use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
        use prost::Message;

        let wire = to_wire("otlp-json", "otlp-protobuf", MINIMAL.as_bytes()).unwrap();
        assert!(!wire.is_empty());
        let back = ExportLogsServiceRequest::decode(&wire[..]).expect("valid protobuf");
        let record = &back.resource_logs[0].scope_logs[0].log_records[0];
        assert_eq!(record.time_unix_nano, 1_755_000_000_123_456_789);
        assert_eq!(record.severity_text, "INFO");
    }

    /// An empty export is ordinary traffic — collectors flush on a timer — and
    /// it encodes to zero bytes, which is a valid empty message and must not be
    /// mistaken for an encoder failure.
    #[test]
    fn an_empty_batch_encodes_to_an_empty_message() {
        let wire = to_wire("otlp-json", "otlp-protobuf", br#"{"resourceLogs":[]}"#).unwrap();
        assert!(wire.is_empty());
    }

    /// A protobuf string field must be valid UTF-8, so the invalid-UTF-8 case
    /// cannot cross that wire at all. The encoder must fail loudly rather than
    /// substituting replacement characters of its own.
    #[test]
    fn invalid_utf8_cannot_be_encoded_as_protobuf() {
        let mut bytes =
            br#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[{"body":{"stringValue":"x"#.to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(br#""}}]}]}]}"#);
        assert!(to_wire("otlp-json", "otlp-protobuf", &bytes).is_err());
    }

    #[test]
    fn an_unknown_conversion_is_an_error_not_a_silent_passthrough() {
        let err = to_wire("otlp-json", "loki-json", b"{}").unwrap_err();
        assert!(format!("{err}").contains("loki-json"), "{err}");
    }

    /// A case lists its preferred encoding first, and the first one both sides
    /// accept wins.
    #[test]
    fn choose_encoding_picks_the_first_the_backend_accepts() {
        let accepted = vec!["otlp-protobuf".to_string()];
        let offered = vec!["otlp-json".to_string(), "otlp-protobuf".to_string()];
        assert_eq!(choose_encoding(&accepted, &offered), Some("otlp-protobuf".into()));
    }

    #[test]
    fn a_backend_that_speaks_the_native_format_gets_it_unconverted() {
        let accepted = vec!["otlp-json".to_string()];
        let offered = vec!["otlp-json".to_string(), "otlp-protobuf".to_string()];
        assert_eq!(choose_encoding(&accepted, &offered), Some("otlp-json".into()));
    }

    /// An adapter that declares nothing accepts everything, so adapters written
    /// before this existed keep working.
    #[test]
    fn an_adapter_declaring_nothing_accepts_the_first_offer() {
        assert_eq!(choose_encoding(&[], &["otlp-json".to_string()]), Some("otlp-json".into()));
    }

    #[test]
    fn no_shared_encoding_yields_none() {
        let accepted = vec!["otlp-protobuf".to_string()];
        let offered = vec!["otlp-json".to_string()];
        assert_eq!(choose_encoding(&accepted, &offered), None);
    }
}
