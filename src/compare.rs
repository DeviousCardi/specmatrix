//! Comparing what a case sent with what a backend returned.
//!
//! A check names the fields that must survive and, optionally, how to read
//! them. Without that, 0.1 produced two stores that returned the same instant
//! as `"2026-09-08T08:36:18.123"` and as `1788856578123456`, and a matrix could
//! not say whether they agreed.
//!
//! Reading a value is not rewriting it. Nothing here changes what a backend
//! stored; it decides how two stored values are compared and what the result
//! line says about them. The comparison rule for an instant is deliberately
//! strict: two instants are equal only when they name the same nanosecond, so
//! a store that dropped digits fails an `exact` check rather than passing one
//! that coerced both sides to the coarser precision first. Coercing would hide
//! exactly the divergence this project exists to find.

use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Compare the JSON values as they stand. The default.
    Raw,
    /// Compare as an instant, reporting the precision each side kept.
    Timestamp,
    /// Compare as an integer, so `42` and `"42"` are the same number.
    Integer,
    /// Compare as a float by bit pattern, so `-0.0` differs from `0.0` and
    /// `NaN` equals `NaN`.
    Float,
    /// Compare as text.
    Text,
}

pub fn kind_from_str(name: &str) -> Result<Kind> {
    Ok(match name {
        "timestamp" => Kind::Timestamp,
        "integer" => Kind::Integer,
        "float" => Kind::Float,
        "string" | "text" => Kind::Text,
        other => anyhow::bail!(
            "unknown `as` value {other:?}; expected `timestamp`, `integer`, `float` or `string`"
        ),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl Precision {
    pub fn name(self) -> &'static str {
        match self {
            Precision::Seconds => "seconds",
            Precision::Milliseconds => "milliseconds",
            Precision::Microseconds => "microseconds",
            Precision::Nanoseconds => "nanoseconds",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instant {
    pub nanos: i128,
    /// The unit the value is expressed in, from its magnitude or its printed
    /// digits.
    pub precision: Precision,
}

impl Instant {
    /// The finest unit the value is a whole multiple of.
    ///
    /// A store can keep a nanosecond field and put a whole number of
    /// microseconds in it, which Quickwit 0.8.2 does. Reporting only the unit
    /// would say it preserved nanoseconds, and the matrix would carry a claim
    /// that is not true. This is a heuristic — a genuine nanosecond instant
    /// ending in three zeros reads as microseconds, one time in a thousand —
    /// so it is reported beside the unit rather than in place of it, and the
    /// corpus's own fixture ends in 123456789 so that it cannot mislead here.
    pub fn resolution(self) -> Precision {
        if self.nanos % 1_000_000_000 == 0 {
            Precision::Seconds
        } else if self.nanos % 1_000_000 == 0 {
            Precision::Milliseconds
        } else if self.nanos % 1_000 == 0 {
            Precision::Microseconds
        } else {
            Precision::Nanoseconds
        }
    }
}

/// Reads an instant from whatever shape a store returned it in.
///
/// Integers are read by magnitude, which is unambiguous for any timestamp
/// between 1973 and 2286: seconds, milliseconds, microseconds and nanoseconds
/// since the epoch occupy disjoint ranges. OTLP sends `timeUnixNano` as a JSON
/// string, so a string of digits is read the same way.
pub fn parse_instant(value: &Value) -> Option<Instant> {
    match value {
        Value::Number(number) => number.as_i64().map(|n| from_magnitude(n as i128)),
        Value::String(text) => {
            let trimmed = text.trim();
            if !trimmed.is_empty()
                && trimmed.strip_prefix('-').unwrap_or(trimmed).bytes().all(|b| b.is_ascii_digit())
            {
                return trimmed.parse::<i128>().ok().map(from_magnitude);
            }
            parse_datetime(trimmed)
        }
        _ => None,
    }
}

fn from_magnitude(value: i128) -> Instant {
    let magnitude = value.abs();
    let (nanos, precision) = if magnitude < 100_000_000_000 {
        (value * 1_000_000_000, Precision::Seconds)
    } else if magnitude < 100_000_000_000_000 {
        (value * 1_000_000, Precision::Milliseconds)
    } else if magnitude < 100_000_000_000_000_000 {
        (value * 1_000, Precision::Microseconds)
    } else {
        (value, Precision::Nanoseconds)
    };
    Instant { nanos, precision }
}

/// Parses a date-time string, with or without an offset. A store that returns
/// `2026-09-08T08:36:18.123` with no zone is read as UTC: it is reporting the
/// instant it was given, and inventing a local zone here would move it.
fn parse_datetime(text: &str) -> Option<Instant> {
    let precision = fractional_precision(text);
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(Instant { nanos: parsed.timestamp_nanos_opt()? as i128, precision });
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return Some(Instant {
                nanos: naive.and_utc().timestamp_nanos_opt()? as i128,
                precision,
            });
        }
    }
    None
}

/// The precision a formatted timestamp actually carries, from the number of
/// fractional digits it prints. A store that renders three of them has told you
/// it kept milliseconds, whatever it holds internally.
fn fractional_precision(text: &str) -> Precision {
    let Some(dot) = text.find('.') else {
        return Precision::Seconds;
    };
    let digits = text[dot + 1..].bytes().take_while(|b| b.is_ascii_digit()).count();
    match digits {
        0 => Precision::Seconds,
        1..=3 => Precision::Milliseconds,
        4..=6 => Precision::Microseconds,
        _ => Precision::Nanoseconds,
    }
}

fn as_i128(value: &Value) -> Option<i128> {
    match value {
        Value::Number(number) => number.as_i64().map(i128::from),
        Value::String(text) => text.trim().parse::<i128>().ok(),
        _ => None,
    }
}

/// Reads a float, including the values JSON cannot carry as numbers. Several
/// checks turn on them: remote-write requires NaN as a stale marker, and `-0.0`
/// is a distinct value a store can silently turn into `0.0`.
fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => match text.trim() {
            "NaN" | "nan" => Some(f64::NAN),
            "Inf" | "+Inf" | "inf" | "+inf" => Some(f64::INFINITY),
            "-Inf" | "-inf" => Some(f64::NEG_INFINITY),
            other => other.parse::<f64>().ok(),
        },
        _ => None,
    }
}

/// Whether two readings are the same value under `kind`.
///
/// A value the reader cannot interpret falls back to raw JSON equality rather
/// than being called equal or unequal on a guess: an unreadable value is a
/// finding about the backend, and hiding it behind a lenient comparison is how
/// a conformance suite becomes worthless.
pub fn equal(sent: &Option<Value>, got: &Option<Value>, kind: Kind) -> bool {
    let (Some(sent), Some(got)) = (sent, got) else {
        return sent.is_none() && got.is_none();
    };
    match kind {
        Kind::Raw => sent == got,
        Kind::Timestamp => match (parse_instant(sent), parse_instant(got)) {
            // The same nanosecond, or not equal. Comparing at the coarser of
            // the two precisions would let a store that dropped digits pass.
            (Some(a), Some(b)) => a.nanos == b.nanos,
            _ => sent == got,
        },
        Kind::Integer => match (as_i128(sent), as_i128(got)) {
            (Some(a), Some(b)) => a == b,
            _ => sent == got,
        },
        Kind::Float => match (as_f64(sent), as_f64(got)) {
            // Bit pattern, not `==`: `NaN != NaN` numerically, and `-0.0 == 0.0`,
            // and both of those are the subject of a check rather than noise.
            (Some(a), Some(b)) => a.to_bits() == b.to_bits(),
            _ => sent == got,
        },
        Kind::Text => match (sent, got) {
            (Value::String(a), Value::String(b)) => a == b,
            _ => sent.to_string() == got.to_string(),
        },
    }
}

/// Renders a value for a result line, with the type it came back as and, for an
/// instant, the precision it was stored at. The type is always shown because
/// 0.1 produced two stores returning the same instant as a string and as a
/// number, and a line without types made them look identical.
pub fn describe(value: &Option<Value>, kind: Kind) -> String {
    let Some(inner) = value else {
        return "<absent>".to_string();
    };
    let json_type = match inner {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    match kind {
        Kind::Timestamp => match parse_instant(inner) {
            Some(instant) => {
                let unit = instant.precision.name();
                let resolution = instant.resolution();
                // Only worth saying when the value carries less than the unit
                // it is expressed in; otherwise the two are the same fact.
                if resolution == instant.precision {
                    format!("{inner} ({json_type}, {unit})")
                } else {
                    format!("{inner} ({json_type}, {unit} holding whole {})", resolution.name())
                }
            }
            None => format!("{inner} ({json_type}, unreadable as an instant)"),
        },
        _ => format!("{inner} ({json_type})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_integer_instant_is_read_by_magnitude() {
        assert_eq!(parse_instant(&json!(1_788_856_578i64)).unwrap().precision, Precision::Seconds);
        assert_eq!(
            parse_instant(&json!(1_788_856_578_123i64)).unwrap().precision,
            Precision::Milliseconds
        );
        assert_eq!(
            parse_instant(&json!(1_788_856_578_123_456i64)).unwrap().precision,
            Precision::Microseconds
        );
        assert_eq!(
            parse_instant(&json!(1_788_856_578_123_456_789i64)).unwrap().precision,
            Precision::Nanoseconds
        );
    }

    /// OTLP encodes int64 as a JSON string, so the value a case sends is a
    /// string of digits and must read as the same instant as the number would.
    #[test]
    fn a_numeric_string_reads_as_the_same_instant_as_the_number() {
        let from_text = parse_instant(&json!("1788856578123456789")).unwrap();
        let from_number = parse_instant(&json!(1_788_856_578_123_456_789i64)).unwrap();
        assert_eq!(from_text, from_number);
        assert_eq!(from_text.nanos, 1_788_856_578_123_456_789);
    }

    /// Parseable returns this shape, with no zone. The precision comes from the
    /// digits it chose to print.
    #[test]
    fn a_datetime_without_a_zone_is_read_as_utc_at_its_printed_precision() {
        let instant = parse_instant(&json!("2026-09-08T08:36:18.123")).unwrap();
        assert_eq!(instant.precision, Precision::Milliseconds);
        assert_eq!(instant.nanos, 1_788_856_578_123_000_000);
    }

    #[test]
    fn an_rfc3339_datetime_is_read_with_its_offset() {
        let utc = parse_instant(&json!("2026-09-08T08:36:18.123456789Z")).unwrap();
        assert_eq!(utc.precision, Precision::Nanoseconds);
        assert_eq!(utc.nanos, 1_788_856_578_123_456_789);
        let offset = parse_instant(&json!("2026-09-08T09:36:18.123456789+01:00")).unwrap();
        assert_eq!(offset.nanos, utc.nanos);
    }

    #[test]
    fn a_datetime_with_no_fraction_is_seconds() {
        assert_eq!(
            parse_instant(&json!("2026-09-08T08:36:18Z")).unwrap().precision,
            Precision::Seconds
        );
    }

    /// The rule that keeps typed comparison from becoming normalisation. This
    /// is the 0.1 divergence, and it must not pass.
    #[test]
    fn an_instant_that_lost_digits_is_not_equal_to_the_one_that_was_sent() {
        let sent = Some(json!("1788856578123456789"));
        let parseable = Some(json!("2026-09-08T08:36:18.123"));
        let openobserve = Some(json!(1_788_856_578_123_456i64));
        assert!(!equal(&sent, &parseable, Kind::Timestamp));
        assert!(!equal(&sent, &openobserve, Kind::Timestamp));
    }

    /// The same instant in two shapes is equal, which is the point of the kind:
    /// a string and a number are not automatically a divergence.
    #[test]
    fn the_same_instant_in_two_shapes_is_equal() {
        let as_string = Some(json!("1788856578123456789"));
        let as_datetime = Some(json!("2026-09-08T08:36:18.123456789Z"));
        assert!(equal(&as_string, &as_datetime, Kind::Timestamp));
    }

    /// Raw comparison is what 0.1 did, and it calls those two a difference.
    #[test]
    fn raw_comparison_still_distinguishes_the_shapes() {
        let as_string = Some(json!("1788856578123456789"));
        let as_datetime = Some(json!("2026-09-08T08:36:18.123456789Z"));
        assert!(!equal(&as_string, &as_datetime, Kind::Raw));
    }

    #[test]
    fn an_integer_compares_across_string_and_number() {
        assert!(equal(&Some(json!("9007199254740993")), &Some(json!(9007199254740993i64)), Kind::Integer));
        assert!(!equal(&Some(json!("42")), &Some(json!(43)), Kind::Integer));
    }

    /// Both of these are the subject of a check rather than noise: remote-write
    /// requires NaN as a stale marker, and a store turning -0.0 into 0.0 has
    /// altered the sample.
    #[test]
    fn floats_compare_by_bit_pattern() {
        assert!(equal(&Some(json!("NaN")), &Some(json!("NaN")), Kind::Float));
        assert!(!equal(&Some(json!("-0.0")), &Some(json!(0.0)), Kind::Float));
        assert!(equal(&Some(json!("-0.0")), &Some(json!(-0.0)), Kind::Float));
        assert!(equal(&Some(json!("+Inf")), &Some(json!("Inf")), Kind::Float));
    }

    /// An absent field is an answer. Absent against absent is equal; absent
    /// against a value is not.
    #[test]
    fn absence_compares_as_a_value() {
        assert!(equal(&None, &None, Kind::Raw));
        assert!(!equal(&None, &Some(json!("x")), Kind::Raw));
        assert!(!equal(&Some(json!("x")), &None, Kind::Timestamp));
    }

    /// A value the reader cannot interpret must not be quietly called equal.
    #[test]
    fn an_unreadable_value_falls_back_to_raw_equality() {
        let a = Some(json!({"nested": true}));
        let b = Some(json!({"nested": true}));
        let c = Some(json!({"nested": false}));
        assert!(equal(&a, &b, Kind::Timestamp));
        assert!(!equal(&a, &c, Kind::Timestamp));
    }

    #[test]
    fn describe_reports_the_type_and_the_precision() {
        assert_eq!(
            describe(&Some(json!("2026-09-08T08:36:18.123")), Kind::Timestamp),
            "\"2026-09-08T08:36:18.123\" (string, milliseconds)"
        );
        assert_eq!(
            describe(&Some(json!(1_788_856_578_123_456i64)), Kind::Timestamp),
            "1788856578123456 (number, microseconds)"
        );
        assert_eq!(describe(&None, Kind::Timestamp), "<absent>");
        assert_eq!(describe(&Some(json!("INFO")), Kind::Raw), "\"INFO\" (string)");
    }

    /// Quickwit 0.8.2 stores a nanosecond field holding a whole number of
    /// microseconds. Reporting only the unit would claim it preserved
    /// nanoseconds, which is a false claim in a published matrix.
    #[test]
    fn a_value_carrying_less_than_its_unit_says_so() {
        assert_eq!(
            describe(&Some(json!(1_788_859_858_123_456_000i64)), Kind::Timestamp),
            "1788859858123456000 (number, nanoseconds holding whole microseconds)"
        );
    }

    #[test]
    fn a_value_that_fills_its_unit_reports_the_unit_alone() {
        assert_eq!(
            describe(&Some(json!(1_788_859_858_123_456_789i64)), Kind::Timestamp),
            "1788859858123456789 (number, nanoseconds)"
        );
    }

    #[test]
    fn resolution_finds_the_finest_unit_a_value_is_a_whole_multiple_of() {
        let at = |n: i128| Instant { nanos: n, precision: Precision::Nanoseconds }.resolution();
        assert_eq!(at(1_788_859_858_000_000_000), Precision::Seconds);
        assert_eq!(at(1_788_859_858_123_000_000), Precision::Milliseconds);
        assert_eq!(at(1_788_859_858_123_456_000), Precision::Microseconds);
        assert_eq!(at(1_788_859_858_123_456_789), Precision::Nanoseconds);
    }

    #[test]
    fn an_unknown_as_value_is_an_error() {
        assert!(kind_from_str("instant").is_err());
        assert_eq!(kind_from_str("timestamp").unwrap(), Kind::Timestamp);
        assert_eq!(kind_from_str("string").unwrap(), Kind::Text);
    }
}
