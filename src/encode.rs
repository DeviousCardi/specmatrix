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
        ("remote-write-json", "remote-write-protobuf") => crate::remote_write::to_wire(payload),
        ("otlp-metrics-json", "otlp-metrics-protobuf") => otlp_metrics_json_to_protobuf(payload),
        ("otlp-traces-json", "otlp-traces-protobuf") => otlp_traces_json_to_protobuf(payload),
        _ => anyhow::bail!("no encoder from {native} to {encoding}"),
    }
}

/// Headers a wire encoding requires, whatever backend is receiving it.
///
/// These belong to the encoding rather than to any adapter: a remote-write
/// receiver identifies the body by `Content-Encoding: snappy` and the protocol
/// version by its own header, and every one of them needs the same three. An
/// adapter that declares a header keeps it — the runner only fills in what is
/// missing — so a store with an unusual content type is still describable, and
/// a new adapter cannot fail in the confusing way an omitted
/// `Content-Encoding` fails.
pub fn headers_for(encoding: &str) -> &'static [(&'static str, &'static str)] {
    match encoding {
        "remote-write-protobuf" => &[
            ("Content-Type", "application/x-protobuf"),
            ("Content-Encoding", "snappy"),
            ("X-Prometheus-Remote-Write-Version", "0.1.0"),
        ],
        "otlp-protobuf" | "otlp-metrics-protobuf" | "otlp-traces-protobuf" => {
            &[("Content-Type", "application/x-protobuf")]
        }
        "otlp-json" | "otlp-metrics-json" | "otlp-traces-json" => {
            &[("Content-Type", "application/json")]
        }
        "es-ndjson" => &[("Content-Type", "application/x-ndjson")],
        _ => &[],
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

/// Metrics take the same route as logs and for the same reason: two of the four
/// metric stores refuse JSON at the Content-Type header, and a corpus that gave
/// up there would report a deviation from a SHOULD as five failed checks.
///
/// The payload is normalised first, and that step is not cosmetic. proto3's
/// canonical JSON writes an int64 as a *string*, because a JSON number cannot
/// hold every int64 exactly, and writes the three values IEEE 754 has and JSON
/// does not as the strings `"NaN"`, `"Infinity"` and `"-Infinity"`. The serde
/// implementation in `opentelemetry-proto` 0.32 accepts none of them: it does
/// not fail, it silently leaves the field unset, and for a histogram `sum` it
/// discards the entire metric.
///
/// Measured, not inferred — see the tests in this module:
///
/// ```text
/// "asDouble":12.5          -> Some(AsDouble(12.5))
/// "asDouble":"NaN"         -> None
/// "asDouble":"Infinity"    -> None
/// "asInt":"9223372036854775807" -> None
/// "asInt":42               -> Some(AsInt(42))
/// "sum":"NaN"              -> the whole metric is None
/// ```
///
/// Every one of those is a value some check exists to send, so without this the
/// runner would quietly export a data point with no value at all. It did, and
/// it produced two false findings before this was caught: GreptimeDB refusing
/// two exports with `No field column found`, and VictoriaMetrics appearing to
/// turn a NaN gauge into zero.
///
/// A second, unrelated gap lives in the same decoder and cost a third finding
/// after this one was already fixed: `ExponentialHistogramDataPoint` is the
/// only data-point message here with no `#[serde(default)]` on its struct, so
/// every field it has — `attributes`, `exemplars`, `startTimeUnixNano`,
/// `scale`, `zeroCount`, `flags`, `zeroThreshold` — must be present in the
/// JSON or the whole point silently decodes to nothing, same as above.
/// `fill_exponential_histogram_defaults` fills them in before the point ever
/// reaches serde. See its own doc for how this one was caught: after the
/// finding it produced had already been filed and had to be withdrawn.
fn otlp_metrics_json_to_protobuf(payload: &[u8]) -> Result<Vec<u8>> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use prost::Message;

    let mut document: serde_json::Value =
        serde_json::from_slice(payload).context("decoding OTLP metrics JSON")?;
    let fixups = normalise_points(&mut document);
    let mut request: ExportMetricsServiceRequest = serde_json::from_value(document)
        .context("decoding OTLP JSON into ExportMetricsServiceRequest")?;
    apply_fixups(&mut request, &fixups);
    Ok(request.encode_to_vec())
}

/// A value the decoder cannot read from JSON, to be set on the message after it
/// has been decoded.
#[derive(Default, Clone)]
struct Fixup {
    value: Option<f64>,
    sum: Option<f64>,
}

/// The five point-carrying fields of a metric, in the order they are visited.
/// A metric carries exactly one, so this order is also the order the decoded
/// message presents them in — which is what lets the fixups be applied
/// positionally.
const POINT_KINDS: [&str; 5] = ["gauge", "sum", "histogram", "exponentialHistogram", "summary"];

/// Rewrites the payload into a form the decoder accepts, returning what has to
/// be put back afterwards, one entry per data point in traversal order.
fn normalise_points(document: &mut serde_json::Value) -> Vec<Fixup> {
    use serde_json::Value;
    let mut fixups = Vec::new();
    let Some(resources) = document.get_mut("resourceMetrics").and_then(Value::as_array_mut) else {
        return fixups;
    };
    for resource in resources {
        let Some(scopes) = resource.get_mut("scopeMetrics").and_then(Value::as_array_mut) else {
            continue;
        };
        for scope in scopes {
            let Some(metrics) = scope.get_mut("metrics").and_then(Value::as_array_mut) else {
                continue;
            };
            for metric in metrics {
                for kind in POINT_KINDS {
                    let Some(points) = metric
                        .get_mut(kind)
                        .and_then(|k| k.get_mut("dataPoints"))
                        .and_then(Value::as_array_mut)
                    else {
                        continue;
                    };
                    for point in points {
                        if kind == "exponentialHistogram" {
                            fill_exponential_histogram_defaults(point);
                        }
                        fixups.push(normalise_point(point));
                    }
                }
            }
        }
    }
    fixups
}

/// Fills in every field `ExponentialHistogramDataPoint` requires but its own
/// struct does not default.
///
/// Every other data-point message here — `Gauge`, `Histogram`,
/// `SummaryDataPoint` — carries `#[serde(default)]`, so a case can write only
/// the fields it cares about. `ExponentialHistogramDataPoint` alone does not,
/// so a missing `attributes`, `exemplars`, `startTimeUnixNano`, `scale`,
/// `zeroCount`, `flags` or `zeroThreshold` fails to deserialize that one
/// variant — and because it sits behind a `#[serde(flatten)]`ed oneof, the
/// failure does not surface as an error. The whole metric decodes with `data:
/// None`, silently, and the case exports a name with nothing behind it.
///
/// Confirmed by encoding, not assumed: the same message with every field
/// listed here present decodes to 50 bytes; missing any one of them silently
/// drops the entire data point. That produced two upstream reports of data
/// loss that were this function's absence, not the stores'.
fn fill_exponential_histogram_defaults(point: &mut serde_json::Value) {
    use serde_json::{json, Value};
    let Some(object) = point.as_object_mut() else { return };
    for (field, default) in [
        ("attributes", json!([])),
        ("startTimeUnixNano", json!("0")),
        ("count", json!("0")),
        ("scale", json!(0)),
        ("zeroCount", json!("0")),
        ("flags", json!(0)),
        ("exemplars", json!([])),
        ("zeroThreshold", json!(0.0)),
    ] {
        object.entry(field).or_insert(default);
    }
    // `positive` and `negative` are optional at the message level, but
    // `Buckets` itself carries no default either: a check that sets one and
    // not the other still needs both fields of the one it sets.
    for side in ["positive", "negative"] {
        if let Some(Value::Object(bucket)) = object.get_mut(side) {
            bucket.entry("offset").or_insert(json!(0));
            bucket.entry("bucketCounts").or_insert(json!([]));
        }
    }
}

fn normalise_point(point: &mut serde_json::Value) -> Fixup {
    use serde_json::Value;
    let mut fixup = Fixup::default();
    // An int64 written as a string is proto3's canonical form. A JSON number
    // holds every i64 exactly, so this one needs no fixup afterwards.
    if let Some(text) = point.get("asInt").and_then(Value::as_str) {
        if let Ok(number) = text.trim().parse::<i64>() {
            point["asInt"] = Value::from(number);
        }
    }
    // A float the decoder cannot read is replaced with zero and put back after
    // decoding. Zero rather than anything else because it is a valid double and
    // keeps the message decodable; nothing reads it in between.
    for (field, slot) in [("asDouble", 0), ("sum", 1)] {
        let Some(text) = point.get(field).and_then(Value::as_str) else {
            continue;
        };
        let Some(parsed) = special_float(text) else {
            continue;
        };
        point[field] = Value::from(0.0);
        if slot == 0 {
            fixup.value = Some(parsed);
        } else {
            fixup.sum = Some(parsed);
        }
    }
    fixup
}

/// The float values JSON has no literal for.
///
/// `NaN`, `Infinity` and `-Infinity` are proto3's canonical spellings. The
/// Prometheus spellings are accepted too, so one corpus does not have to write
/// the same value two ways depending on which suite a case is in.
fn special_float(text: &str) -> Option<f64> {
    match text.trim() {
        "NaN" => Some(f64::NAN),
        "Infinity" | "+Inf" | "Inf" => Some(f64::INFINITY),
        "-Infinity" | "-Inf" => Some(f64::NEG_INFINITY),
        _ => None,
    }
}

/// Puts the unreadable values back, walking the decoded message in the same
/// order `normalise_points` walked the JSON.
fn apply_fixups(
    request: &mut opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest,
    fixups: &[Fixup],
) {
    use opentelemetry_proto::tonic::metrics::v1::{metric::Data, number_data_point};
    let mut next = fixups.iter();
    for resource in &mut request.resource_metrics {
        for scope in &mut resource.scope_metrics {
            for metric in &mut scope.metrics {
                match metric.data.as_mut() {
                    Some(Data::Gauge(gauge)) => {
                        for point in &mut gauge.data_points {
                            let Some(fixup) = next.next() else { return };
                            if let Some(value) = fixup.value {
                                point.value = Some(number_data_point::Value::AsDouble(value));
                            }
                        }
                    }
                    Some(Data::Sum(sum)) => {
                        for point in &mut sum.data_points {
                            let Some(fixup) = next.next() else { return };
                            if let Some(value) = fixup.value {
                                point.value = Some(number_data_point::Value::AsDouble(value));
                            }
                        }
                    }
                    Some(Data::Histogram(histogram)) => {
                        for point in &mut histogram.data_points {
                            let Some(fixup) = next.next() else { return };
                            if let Some(value) = fixup.sum {
                                point.sum = Some(value);
                            }
                        }
                    }
                    Some(Data::ExponentialHistogram(histogram)) => {
                        for point in &mut histogram.data_points {
                            let Some(fixup) = next.next() else { return };
                            if let Some(value) = fixup.sum {
                                point.sum = Some(value);
                            }
                        }
                    }
                    Some(Data::Summary(summary)) => {
                        for point in &mut summary.data_points {
                            let Some(fixup) = next.next() else { return };
                            if let Some(value) = fixup.sum {
                                point.sum = value;
                            }
                        }
                    }
                    None => {}
                }
            }
        }
    }
}

/// Traces need no fixup step. `Span`, unlike `ExponentialHistogramDataPoint`,
/// carries `#[serde(default)]` on every message in its tree, so a case can
/// omit any field it does not care about and the decoder fills in the zero
/// value rather than discarding the whole span — confirmed by reading the
/// generated types rather than assumed, after the exponential-histogram
/// lesson from the metrics encoder.
fn otlp_traces_json_to_protobuf(payload: &[u8]) -> Result<Vec<u8>> {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message;

    let request: ExportTraceServiceRequest = serde_json::from_slice(payload)
        .context("decoding OTLP JSON into ExportTraceServiceRequest")?;
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

    /// Every value the corpus writes must survive to the wire. These are the
    /// exact forms `opentelemetry-proto` 0.32's serde drops on the floor, and
    /// each one is the subject of a check, so a regression here would quietly
    /// turn those checks into assertions about an empty data point.
    #[test]
    fn otlp_metrics_json_carries_the_values_json_cannot_write_as_numbers() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{metric::Data, number_data_point};
        use prost::Message;

        let gauge = |value: &str| {
            format!(
                concat!(
                    r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[{{"name":"m","#,
                    r#""gauge":{{"dataPoints":[{{"timeUnixNano":"1",{}}}]}}}}]}}]}}]}}"#
                ),
                value
            )
        };
        let read = |json: String| {
            let wire = to_wire("otlp-metrics-json", "otlp-metrics-protobuf", json.as_bytes())
                .expect("encodes");
            let back = ExportMetricsServiceRequest::decode(&wire[..]).expect("valid protobuf");
            match back.resource_metrics[0].scope_metrics[0].metrics[0].data.clone() {
                Some(Data::Gauge(g)) => g.data_points[0].value,
                other => panic!("expected a gauge, got {other:?}"),
            }
        };

        match read(gauge(r#""asDouble":"NaN""#)) {
            Some(number_data_point::Value::AsDouble(v)) => assert!(v.is_nan()),
            other => panic!("NaN was lost: {other:?}"),
        }
        match read(gauge(r#""asDouble":"Infinity""#)) {
            Some(number_data_point::Value::AsDouble(v)) => assert_eq!(v, f64::INFINITY),
            other => panic!("+Inf was lost: {other:?}"),
        }
        match read(gauge(r#""asDouble":"-Infinity""#)) {
            Some(number_data_point::Value::AsDouble(v)) => assert_eq!(v, f64::NEG_INFINITY),
            other => panic!("-Inf was lost: {other:?}"),
        }
        // An int64 at its maximum, written as proto3 writes it. A JSON number
        // cannot hold this exactly, which is why the spec writes it as a string
        // and why losing it here would have been invisible.
        match read(gauge(r#""asInt":"9223372036854775807""#)) {
            Some(number_data_point::Value::AsInt(v)) => assert_eq!(v, i64::MAX),
            other => panic!("int64 max was lost: {other:?}"),
        }
        // Negative zero, which the remote-write wire format cannot carry at all.
        // OTLP JSON can, and this is the encoding that lets the check exist.
        match read(gauge(r#""asDouble":-0.0"#)) {
            Some(number_data_point::Value::AsDouble(v)) => {
                assert_eq!(v.to_bits(), (-0.0f64).to_bits())
            }
            other => panic!("negative zero was lost: {other:?}"),
        }
    }

    /// `ExponentialHistogramDataPoint` requires every field present, unlike
    /// every other data-point message here — see the module doc. A minimal,
    /// natural-looking payload (name, temporality, one data point with count,
    /// sum, scale, zeroCount and one bucket) omits `attributes`, `exemplars`
    /// and `zeroThreshold`, and without the fixup that is enough to make the
    /// whole metric decode to nothing.
    #[test]
    fn an_exponential_histogram_with_only_the_natural_fields_still_decodes() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::metric::Data;
        use prost::Message;

        let json = concat!(
            r#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[{"name":"m","#,
            r#""exponentialHistogram":{"aggregationTemporality":2,"dataPoints":[{"#,
            r#""timeUnixNano":"1","count":"3","sum":6.0,"scale":0,"zeroCount":"0","#,
            r#""positive":{"offset":0,"bucketCounts":["1","2"]}}]}}]}]}]}"#
        );
        let wire = to_wire("otlp-metrics-json", "otlp-metrics-protobuf", json.as_bytes())
            .expect("encodes");
        let back = ExportMetricsServiceRequest::decode(&wire[..]).expect("valid protobuf");
        match back.resource_metrics[0].scope_metrics[0].metrics[0].data.clone() {
            Some(Data::ExponentialHistogram(h)) => {
                let point = &h.data_points[0];
                assert_eq!(point.count, 3);
                assert_eq!(point.sum, Some(6.0));
                assert_eq!(point.positive.as_ref().unwrap().bucket_counts, vec![1, 2]);
            }
            other => panic!("the metric was discarded: {other:?}"),
        }
    }

    /// A histogram `sum` of NaN made the decoder discard the whole metric, not
    /// just the field, which is the most dangerous shape of this bug: the
    /// export still encodes, and it carries nothing.
    #[test]
    fn a_histogram_sum_of_nan_does_not_discard_the_metric() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::metric::Data;
        use prost::Message;

        let json = concat!(
            r#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[{"name":"m","#,
            r#""histogram":{"aggregationTemporality":2,"dataPoints":[{"timeUnixNano":"1","#,
            r#""count":"3","sum":"NaN"}]}}]}]}]}"#
        );
        let wire = to_wire("otlp-metrics-json", "otlp-metrics-protobuf", json.as_bytes())
            .expect("encodes");
        let back = ExportMetricsServiceRequest::decode(&wire[..]).expect("valid protobuf");
        match back.resource_metrics[0].scope_metrics[0].metrics[0].data.clone() {
            Some(Data::Histogram(h)) => {
                assert_eq!(h.data_points[0].count, 3);
                assert!(h.data_points[0].sum.expect("sum present").is_nan());
            }
            other => panic!("the metric was discarded: {other:?}"),
        }
    }

    /// Fixups are applied positionally, so an export carrying several points
    /// must put each value back on the point it came from.
    #[test]
    fn fixups_land_on_the_points_they_came_from() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{metric::Data, number_data_point::Value};
        use prost::Message;

        let json = concat!(
            r#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":["#,
            r#"{"name":"a","gauge":{"dataPoints":[{"timeUnixNano":"1","asDouble":1.5},"#,
            r#"{"timeUnixNano":"2","asDouble":"NaN"}]}},"#,
            r#"{"name":"b","gauge":{"dataPoints":[{"timeUnixNano":"3","asDouble":"-Infinity"}]}}"#,
            r#"]}]}]}"#
        );
        let wire = to_wire("otlp-metrics-json", "otlp-metrics-protobuf", json.as_bytes())
            .expect("encodes");
        let back = ExportMetricsServiceRequest::decode(&wire[..]).expect("valid protobuf");
        let metrics = &back.resource_metrics[0].scope_metrics[0].metrics;
        let Some(Data::Gauge(a)) = metrics[0].data.clone() else { panic!("a is not a gauge") };
        let Some(Data::Gauge(b)) = metrics[1].data.clone() else { panic!("b is not a gauge") };
        assert!(matches!(a.data_points[0].value, Some(Value::AsDouble(v)) if v == 1.5));
        assert!(matches!(a.data_points[1].value, Some(Value::AsDouble(v)) if v.is_nan()));
        assert!(
            matches!(b.data_points[0].value, Some(Value::AsDouble(v)) if v == f64::NEG_INFINITY)
        );
    }

    #[test]
    fn remote_write_json_becomes_a_snappy_block() {
        let wire = to_wire(
            "remote-write-json",
            "remote-write-protobuf",
            br#"{"timeseries":[{"labels":[{"name":"__name__","value":"g"}],
                "samples":[{"value":1,"timestamp":1}]}]}"#,
        )
        .unwrap();
        assert!(snap::raw::Decoder::new().decompress_vec(&wire).is_ok());
    }

    /// A remote-write body is unidentifiable without `Content-Encoding:
    /// snappy`, and a receiver answers a confusing decompression error rather
    /// than naming the missing header. Every adapter needs all three, so the
    /// encoding carries them rather than each adapter repeating them.
    #[test]
    fn remote_write_carries_the_three_headers_a_receiver_needs() {
        let headers = headers_for("remote-write-protobuf");
        assert_eq!(headers.len(), 3);
        assert!(headers.contains(&("Content-Encoding", "snappy")));
        assert!(headers.contains(&("X-Prometheus-Remote-Write-Version", "0.1.0")));
    }

    #[test]
    fn an_encoding_with_no_required_headers_asks_for_none() {
        assert!(headers_for("loki-json").is_empty());
    }
}
