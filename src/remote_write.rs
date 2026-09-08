//! Prometheus remote-write 1.0: the four messages of its write path, and the
//! JSON form the corpus keeps its payloads in.
//!
//! Remote-write is snappy-compressed protobuf and nothing else. There is no
//! JSON on the wire, so a corpus that kept its payloads in the wire format
//! would be a corpus of opaque binaries: a maintainer could not read a case,
//! could not diff two of them, and could not send one by hand. The cases are
//! therefore written as JSON and encoded here.
//!
//! The messages are declared by hand rather than generated from the `.proto`.
//! `prost-build` needs `protoc` on the build machine, and the matrix has to
//! reproduce from a clean machine with only Docker and Rust — a build
//! dependency on a C++ toolchain would quietly make that false. These four
//! messages are the whole of the 1.0 write path; a case that later needs
//! metadata or exemplars adds the field with its tag, which is cheaper than
//! taking on a code generator.
//!
//! One thing the wire format cannot do, worth knowing before reading any
//! remote-write row: it cannot carry `-0.0`. proto3 omits a scalar field
//! holding its default value and `-0.0 == 0.0`, so the sign is gone before the
//! request leaves any sender. See the test that records the exact bytes.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

/// The NaN payload Prometheus reserves for staleness, from its own
/// `pkg/value`. Written as a bit pattern because no float literal names it and
/// because it must be exactly this one: any other NaN is an ordinary value.
pub const STALE_NAN: u64 = 0x7ff0_0000_0000_0002;

#[derive(Clone, PartialEq, prost::Message)]
pub struct WriteRequest {
    #[prost(message, repeated, tag = "1")]
    pub timeseries: Vec<TimeSeries>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TimeSeries {
    #[prost(message, repeated, tag = "1")]
    pub labels: Vec<Label>,
    #[prost(message, repeated, tag = "2")]
    pub samples: Vec<Sample>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Label {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Sample {
    #[prost(double, tag = "1")]
    pub value: f64,
    #[prost(int64, tag = "2")]
    pub timestamp: i64,
}

/// The JSON form, `remote-write-json`. Documented in `docs/TEST-CASES.md`;
/// the rules that matter are enforced here rather than described.
#[derive(Debug, Deserialize)]
struct JsonWrite {
    #[serde(default)]
    timeseries: Vec<JsonSeries>,
}

#[derive(Debug, Deserialize)]
struct JsonSeries {
    #[serde(default)]
    labels: Vec<JsonLabel>,
    #[serde(default)]
    samples: Vec<JsonSample>,
}

#[derive(Debug, Deserialize)]
struct JsonLabel {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct JsonSample {
    value: Value,
    timestamp: Value,
}

/// Parses the JSON form into the protobuf messages.
///
/// Label order is preserved exactly as written. The specification requires
/// senders to sort labels by name, and whether a receiver enforces that is one
/// of the checks — so a case that wants unsorted labels has to be able to send
/// unsorted labels, and this function must not helpfully fix them.
pub fn parse(payload: &[u8]) -> Result<WriteRequest> {
    let parsed: JsonWrite =
        serde_json::from_slice(payload).context("decoding remote-write-json")?;
    let mut timeseries = Vec::with_capacity(parsed.timeseries.len());
    for series in parsed.timeseries {
        let labels =
            series.labels.into_iter().map(|l| Label { name: l.name, value: l.value }).collect();
        let mut samples = Vec::with_capacity(series.samples.len());
        for sample in series.samples {
            samples.push(Sample {
                value: sample_value(&sample.value)?,
                timestamp: sample_timestamp(&sample.timestamp)?,
            });
        }
        timeseries.push(TimeSeries { labels, samples });
    }
    Ok(WriteRequest { timeseries })
}

/// Encodes and snappy-compresses, which is what a receiver expects on the wire.
///
/// Raw block format, not the framed stream format. Remote-write 1.0 specifies
/// the block format, and a framed body is rejected by every receiver — with an
/// error that names decompression rather than the encoder, which is a slow way
/// to find this out.
pub fn to_wire(payload: &[u8]) -> Result<Vec<u8>> {
    use prost::Message;
    let encoded = parse(payload)?.encode_to_vec();
    Ok(snap::raw::Encoder::new().compress_vec(&encoded)?)
}

/// Reads a sample value, including the three IEEE 754 values JSON has no
/// literal for.
///
/// Every one of them is the subject of a check. NaN is how remote-write marks a
/// series stale, so a receiver that treats it as malformed rejects ordinary
/// traffic, and `+Inf` is the required upper bucket bound of every histogram.
/// Writing them as strings is the only way a JSON corpus can carry them at all.
///
/// `-0.0` is accepted and read correctly here, but proto3 erases it at the
/// encoding step — see the module documentation. It stays accepted because the
/// format is shared, and because a parser that silently normalised it would
/// make that loss invisible instead of testable.
fn sample_value(value: &Value) -> Result<f64> {
    match value {
        Value::Number(number) => {
            number.as_f64().with_context(|| format!("sample value {number} is not a float"))
        }
        Value::String(text) => match text.trim() {
            "NaN" => Ok(f64::NAN),
            // Prometheus marks a series stale with one specific NaN payload,
            // not with any NaN. Confirmed against prom/prometheus:v3.6.0 on
            // 2026-09-09: an ordinary NaN sample is stored and read back as
            // "NaN", while this one ends the series. A corpus that could only
            // send the ordinary one could not test staleness at all, and a
            // corpus that conflated them would report every store that keeps a
            // NaN as failing to honour a stale marker it was never sent.
            "StaleNaN" => Ok(f64::from_bits(STALE_NAN)),
            "+Inf" | "Inf" => Ok(f64::INFINITY),
            "-Inf" => Ok(f64::NEG_INFINITY),
            "-0.0" | "-0" => Ok(-0.0),
            // Not a general string-to-float fallback. A case that means a
            // number writes a number; accepting `"12"` here would let a typo
            // pass as data and make the corpus's own format ambiguous.
            other => bail!(
                "sample value {other:?} is not a number; the only strings \
                 permitted are \"NaN\", \"StaleNaN\", \"+Inf\", \"-Inf\" and \"-0.0\""
            ),
        },
        other => bail!("sample value must be a number or a special-value string, got {other}"),
    }
}

/// Reads a sample timestamp, in milliseconds as remote-write defines it.
///
/// A string is accepted because a template variable renders as one, and
/// `{{ now_ms }}` in a payload is what keeps the corpus from ageing out of a
/// store's ingest window.
fn sample_timestamp(value: &Value) -> Result<i64> {
    match value {
        Value::Number(number) => {
            number.as_i64().with_context(|| format!("timestamp {number} is not an integer"))
        }
        Value::String(text) => text
            .trim()
            .parse::<i64>()
            .with_context(|| format!("timestamp {text:?} is not an integer")),
        other => bail!("timestamp must be an integer in milliseconds, got {other}"),
    }
}

/// Reads a logical field out of a payload the corpus sent, so it can be
/// compared with what a store returned.
///
/// `series` names which series the check asserts on, by its `__name__`. A
/// request carries several — the whole point of `histogram-nan-count` is that
/// one stale series must not cost the unrelated one sent beside it — so a check
/// that did not say which would be asserting on whichever happened to be first.
pub fn logical_field(sent: &Value, field: &str, series: Option<&str>) -> Option<Value> {
    let all = sent.get("timeseries")?.as_array()?;
    let chosen = match series {
        Some(name) => all.iter().find(|s| metric_name(s) == Some(name))?,
        None if all.len() == 1 => &all[0],
        // Deliberately not "the first one". A request with several series and a
        // check that did not name one is an under-specified check, and
        // guessing would give it a verdict it has not earned.
        None => return None,
    };
    match field {
        "labels" => {
            let mut map = serde_json::Map::new();
            for label in chosen.get("labels")?.as_array()? {
                let (Some(name), Some(value)) =
                    (label.get("name")?.as_str(), label.get("value")?.as_str())
                else {
                    continue;
                };
                map.insert(name.to_string(), Value::String(value.to_string()));
            }
            Some(Value::Object(map))
        }
        "value" => latest_sample(chosen)?.get("value").cloned(),
        "timestamp" => latest_sample(chosen)?.get("timestamp").cloned(),
        other => chosen.get(other).cloned(),
    }
}

/// The sample an instant query would return: the one with the greatest
/// timestamp.
///
/// Not `samples[0]`. Read-back for this protocol is an instant query, which
/// answers with the most recent sample at or before the query time, so the
/// comparable value on the sent side is the latest one. Taking the first
/// reported `stale-marker-then-resume` as an ALTER on Prometheus — sent 41,
/// read back 43 — when 43 was exactly what the check meant to assert.
///
/// Timestamps are compared as written, without sorting the payload: a case
/// that sends samples out of order is testing that, and reordering them here
/// would make the check assert that the harness sorts.
fn latest_sample(series: &Value) -> Option<&Value> {
    series
        .get("samples")?
        .as_array()?
        .iter()
        .max_by_key(|s| s.get("timestamp").and_then(timestamp_of).unwrap_or(i64::MIN))
}

fn timestamp_of(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// A PromQL selector naming one series of this run.
///
/// Two forms, because PromQL has two. A metric name that is a legacy
/// identifier goes in front of the braces; anything else — a name with dots,
/// which is what an OpenTelemetry metric bridged into Prometheus looks like —
/// can only be named by the quoted form inside them. Building the wrong one
/// produces a PromQL syntax error, which the runner sees as an empty result
/// and reports as the store having lost the record: a harness bug wearing a
/// finding's clothes, and one this project made before catching it.
pub fn series_selector(name: &str, run_key_field: Option<&str>, run_key: &str) -> String {
    let matcher = run_key_field.map(|f| format!("{f}=\"{run_key}\"")).unwrap_or_default();
    if is_legacy_name(name) {
        format!("{name}{{{matcher}}}")
    } else {
        let separator = if matcher.is_empty() { "" } else { "," };
        format!("{{\"{name}\"{separator}{matcher}}}")
    }
}

/// Whether a metric name matches `[a-zA-Z_:][a-zA-Z0-9_:]*`, the only shape
/// PromQL accepts outside the braces.
fn is_legacy_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

fn metric_name(series: &Value) -> Option<&str> {
    series
        .get("labels")?
        .as_array()?
        .iter()
        .find(|l| l.get("name").and_then(Value::as_str) == Some("__name__"))?
        .get("value")?
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    const GAUGE: &str = r#"{"timeseries":[{"labels":[
        {"name":"__name__","value":"specmatrix_gauge"},
        {"name":"specmatrix_run","value":"sm-abc"}],
        "samples":[{"value":12.5,"timestamp":"1757000000000"}]}]}"#;

    fn round_trip(payload: &str) -> WriteRequest {
        let wire = to_wire(payload.as_bytes()).expect("encodes");
        let raw = snap::raw::Decoder::new().decompress_vec(&wire).expect("snappy block");
        WriteRequest::decode(&raw[..]).expect("valid protobuf")
    }

    /// Prints the exact bytes of the duplicate-label payload, base64-encoded,
    /// so an upstream report can be reproduced with curl and nothing else.
    /// Ignored by default; run with `cargo test emit_duplicate_label_body --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn emit_duplicate_label_body() {
        let payload = br#"{"timeseries":[{"labels":[
            {"name":"__name__","value":"dup_demo"},
            {"name":"zone","value":"a"},
            {"name":"zone","value":"b"}],
            "samples":[{"value":1,"timestamp":TS}]}]}"#;
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let text = String::from_utf8_lossy(payload).replace("TS", &now.to_string());
        let wire = to_wire(text.as_bytes()).unwrap();
        eprintln!("{}", base64(&wire));
    }

    /// Local base64, so the repro helper adds no dependency to the crate.
    #[cfg(test)]
    fn base64(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn a_gauge_survives_the_json_to_protobuf_step() {
        let request = round_trip(GAUGE);
        let series = &request.timeseries[0];
        assert_eq!(series.labels[0].name, "__name__");
        assert_eq!(series.labels[0].value, "specmatrix_gauge");
        assert_eq!(series.samples[0].value, 12.5);
        assert_eq!(series.samples[0].timestamp, 1_757_000_000_000);
    }

    /// NaN is how a sender marks a series stale. If it did not survive the
    /// encoder, every staleness check would be measuring this function.
    #[test]
    fn nan_survives_as_nan() {
        let request = round_trip(
            r#"{"timeseries":[{"labels":[{"name":"__name__","value":"g"}],
                "samples":[{"value":"NaN","timestamp":1}]}]}"#,
        );
        assert!(request.timeseries[0].samples[0].value.is_nan());
    }

    /// Staleness turns on one exact bit pattern, so this asserts on the bits
    /// rather than on `is_nan`. A quiet NaN would pass an `is_nan` check and
    /// mean something entirely different to a receiver.
    #[test]
    fn a_stale_marker_is_the_one_reserved_nan_payload() {
        let request = round_trip(
            r#"{"timeseries":[{"labels":[{"name":"__name__","value":"g"}],
                "samples":[{"value":"StaleNaN","timestamp":1}]}]}"#,
        );
        let value = request.timeseries[0].samples[0].value;
        assert_eq!(value.to_bits(), STALE_NAN);
    }

    /// An ordinary NaN must not be a stale marker, or every check about
    /// staleness would be satisfied by traffic that has nothing to do with it.
    #[test]
    fn an_ordinary_nan_is_not_the_stale_payload() {
        let request = round_trip(
            r#"{"timeseries":[{"labels":[{"name":"__name__","value":"g"}],
                "samples":[{"value":"NaN","timestamp":1}]}]}"#,
        );
        assert_ne!(request.timeseries[0].samples[0].value.to_bits(), STALE_NAN);
    }

    #[test]
    fn both_infinities_survive_with_their_signs() {
        let request = round_trip(
            r#"{"timeseries":[
                {"labels":[{"name":"__name__","value":"a"}],"samples":[{"value":"+Inf","timestamp":1}]},
                {"labels":[{"name":"__name__","value":"b"}],"samples":[{"value":"-Inf","timestamp":1}]}]}"#,
        );
        assert_eq!(request.timeseries[0].samples[0].value, f64::INFINITY);
        assert_eq!(request.timeseries[1].samples[0].value, f64::NEG_INFINITY);
    }

    /// Remote-write cannot carry a negative zero, and this records why.
    ///
    /// Confirmed by encoding, not assumed: `Sample { value: -0.0, timestamp: 1 }`
    /// encodes to `[18, 2, 16, 1]` — the length-delimited sample containing the
    /// timestamp and nothing else. proto3 omits a scalar field holding its
    /// default value, `-0.0 == 0.0` is true, so the value is elided and the
    /// receiver reads the field's default, `+0.0`.
    ///
    /// This is a property of the protocol, not of any receiver. Every
    /// conformant sender loses the sign the same way, Prometheus's own
    /// included, so there is deliberately no `negative-zero-gauge` case in the
    /// remote-write suite: it would blame a store for something it never had
    /// the chance to keep. The encoder therefore does what every other sender
    /// does rather than hand-encoding the field to preserve a sign that real
    /// traffic never carries. The check lives in the OTLP-metrics suite
    /// instead, where the JSON encoding can express it.
    #[test]
    fn negative_zero_cannot_cross_the_remote_write_wire() {
        for written in [r#""-0.0""#, "-0.0"] {
            let request = round_trip(&format!(
                r#"{{"timeseries":[{{"labels":[{{"name":"__name__","value":"g"}}],
                    "samples":[{{"value":{written},"timestamp":1}}]}}]}}"#
            ));
            let value = request.timeseries[0].samples[0].value;
            assert_eq!(value.to_bits(), 0.0f64.to_bits(), "written as {written}");
        }
    }

    /// The sign is present up to the moment of encoding, so the loss is the
    /// wire format's and not this parser's.
    #[test]
    fn the_parser_reads_the_sign_even_though_the_wire_drops_it() {
        let parsed =
            parse(br#"{"timeseries":[{"labels":[],"samples":[{"value":"-0.0","timestamp":1}]}]}"#)
                .unwrap();
        assert_eq!(parsed.timeseries[0].samples[0].value.to_bits(), (-0.0f64).to_bits());
    }

    /// A check for `unsorted-labels` can only exist if the encoder sends them
    /// unsorted. Sorting here would make that check assert that the harness
    /// sorts, which every receiver would pass.
    #[test]
    fn label_order_is_preserved_exactly_as_written() {
        let request = round_trip(
            r#"{"timeseries":[{"labels":[
                {"name":"zzz","value":"1"},
                {"name":"__name__","value":"g"},
                {"name":"aaa","value":"2"}],
                "samples":[{"value":1,"timestamp":1}]}]}"#,
        );
        let names: Vec<&str> =
            request.timeseries[0].labels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["zzz", "__name__", "aaa"]);
    }

    /// Zero timeseries is a valid `WriteRequest` and encodes to zero bytes.
    /// Snappy still wraps it, so the body is not empty — a receiver that
    /// rejects it is rejecting a message its own encoder produces.
    #[test]
    fn an_empty_write_request_still_encodes() {
        let wire = to_wire(br#"{"timeseries":[]}"#).expect("encodes");
        let raw = snap::raw::Decoder::new().decompress_vec(&wire).expect("snappy block");
        assert!(raw.is_empty());
        assert_eq!(WriteRequest::decode(&raw[..]).unwrap().timeseries.len(), 0);
    }

    #[test]
    fn an_unknown_special_value_is_an_error_not_a_zero() {
        let err = to_wire(
            br#"{"timeseries":[{"labels":[],"samples":[{"value":"twelve","timestamp":1}]}]}"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("twelve"), "{err:#}");
    }

    /// The corpus writes its own numbers as numbers. Accepting `"12"` would
    /// make a typo indistinguishable from data.
    #[test]
    fn a_numeric_string_value_is_refused() {
        assert!(to_wire(
            br#"{"timeseries":[{"labels":[],"samples":[{"value":"12","timestamp":1}]}]}"#
        )
        .is_err());
    }

    #[test]
    fn compression_is_the_block_format_not_the_framed_one() {
        let wire = to_wire(GAUGE.as_bytes()).unwrap();
        // The framed format starts with the stream identifier chunk 0xff
        // followed by "sNaPpY". A receiver decompressing with the block decoder
        // fails on that, and the error names decompression rather than us.
        assert_ne!(wire.first(), Some(&0xff));
        assert!(snap::raw::Decoder::new().decompress_vec(&wire).is_ok());
    }

    #[test]
    fn a_named_series_is_the_one_read() {
        let sent: Value = serde_json::from_str(
            r#"{"timeseries":[
                {"labels":[{"name":"__name__","value":"stale"}],"samples":[{"value":"NaN","timestamp":1}]},
                {"labels":[{"name":"__name__","value":"unrelated_gauge"},{"name":"specmatrix_run","value":"sm-abc"}],
                 "samples":[{"value":7,"timestamp":1}]}]}"#,
        )
        .unwrap();
        assert_eq!(logical_field(&sent, "value", Some("unrelated_gauge")), Some(Value::from(7)));
        assert_eq!(
            logical_field(&sent, "labels", Some("unrelated_gauge")),
            Some(serde_json::json!({"__name__":"unrelated_gauge","specmatrix_run":"sm-abc"}))
        );
    }

    /// Two series and no `series:` is an under-specified check. Reading the
    /// first would give it a verdict about a series it never named.
    #[test]
    fn several_series_without_a_name_reads_nothing() {
        let sent: Value = serde_json::from_str(
            r#"{"timeseries":[
                {"labels":[{"name":"__name__","value":"a"}],"samples":[{"value":1,"timestamp":1}]},
                {"labels":[{"name":"__name__","value":"b"}],"samples":[{"value":2,"timestamp":1}]}]}"#,
        )
        .unwrap();
        assert_eq!(logical_field(&sent, "value", None), None);
    }

    #[test]
    fn one_series_needs_no_name() {
        let sent: Value = serde_json::from_str(GAUGE).unwrap();
        assert_eq!(logical_field(&sent, "value", None), Some(Value::from(12.5)));
    }

    #[test]
    fn the_value_read_is_the_one_an_instant_query_would_return() {
        let sent: Value = serde_json::from_str(
            r#"{"timeseries":[{"labels":[{"name":"__name__","value":"g"}],"samples":[
                {"value":41,"timestamp":"1000"},
                {"value":"StaleNaN","timestamp":"2000"},
                {"value":43,"timestamp":"3000"}]}]}"#,
        )
        .unwrap();
        assert_eq!(logical_field(&sent, "value", None), Some(Value::from(43)));
    }

    /// Samples sent out of order stay out of order on the wire — that is the
    /// point of `out-of-order-samples-one-series` — and the latest is still
    /// the one an instant query answers with.
    #[test]
    fn out_of_order_samples_are_not_reordered_but_the_latest_still_wins() {
        let sent: Value = serde_json::from_str(
            r#"{"timeseries":[{"labels":[{"name":"__name__","value":"g"}],"samples":[
                {"value":2,"timestamp":2000},{"value":1,"timestamp":1000}]}]}"#,
        )
        .unwrap();
        assert_eq!(logical_field(&sent, "value", None), Some(Value::from(2)));
        assert_eq!(
            parse(sent.to_string().as_bytes()).unwrap().timeseries[0].samples[0].timestamp,
            2000
        );
    }

    #[test]
    fn a_legacy_name_goes_in_front_of_the_braces() {
        assert_eq!(
            series_selector("specmatrix_gauge", Some("specmatrix_run"), "sm-1"),
            r#"specmatrix_gauge{specmatrix_run="sm-1"}"#
        );
    }

    /// A dotted name is not a PromQL identifier. Written the legacy way it is
    /// a syntax error, and an empty result that reads as lost data.
    #[test]
    fn a_dotted_name_uses_the_quoted_form() {
        assert_eq!(
            series_selector("specmatrix.utf8.gauge", Some("specmatrix_run"), "sm-1"),
            r#"{"specmatrix.utf8.gauge",specmatrix_run="sm-1"}"#
        );
    }

    #[test]
    fn an_adapter_with_no_run_key_field_still_gets_a_valid_selector() {
        assert_eq!(series_selector("g", None, "sm-1"), "g{}");
        assert_eq!(series_selector("a.b", None, "sm-1"), r#"{"a.b"}"#);
    }
}
