//! Query-semantics checks: load a small dataset, run one query, compare which
//! records come back.
//!
//! `docs/DESIGN.md` lists four ways a backend mishandles data and this is the
//! fourth — the data is intact, but the same query returns different rows.
//! `docs/TEST-CASES.md` calls it the largest source of silent divergence, and
//! it needs its own shape: a round-trip check compares one record's fields,
//! this compares a set of records.
//!
//! The marker is a field inside each document, never the store's own document
//! id. Whether a backend honours an id supplied at write time is a finding in
//! its own right, and keying on it would make every query case in the suite
//! fail for that one reason — eight findings out of one fact, which is the
//! mistake 0.1 caught with Quickwit's content-type.

use std::collections::BTreeSet;

use serde_json::Value;

/// The marker of each row a query returned, in the order the backend gave them.
pub fn returned_markers(response: &Value, records: &str, marker_field: &str) -> Vec<String> {
    let node = if records.is_empty() { Some(response) } else { response.pointer(records) };
    let Some(Value::Array(rows)) = node else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| row.pointer(marker_field))
        .map(|marker| match marker {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect()
}

/// Compares two row sets, ignoring order. `None` when they agree, or a detail
/// line naming what differs and in which direction.
///
/// Both directions matter and they mean different things. A missing row is a
/// query that failed to match something it should have; an unexpected row is a
/// query that matched something it should not, which is the shape of every
/// absent-field negation bug the corpus cites.
pub fn compare(expected: &[String], actual: &[String]) -> Option<String> {
    let want: BTreeSet<&String> = expected.iter().collect();
    let got: BTreeSet<&String> = actual.iter().collect();
    if want == got {
        return None;
    }
    let mut parts = Vec::new();
    let missing: Vec<&str> = want.difference(&got).map(|s| s.as_str()).collect();
    let unexpected: Vec<&str> = got.difference(&want).map(|s| s.as_str()).collect();
    if !missing.is_empty() {
        parts.push(format!("missing: {}", missing.join(", ")));
    }
    if !unexpected.is_empty() {
        parts.push(format!("unexpected: {}", unexpected.join(", ")));
    }
    Some(parts.join("; "))
}

/// Compares two row sets as sequences, for a check that asserts an ordering the
/// protocol guarantees.
pub fn compare_ordered(expected: &[String], actual: &[String]) -> Option<String> {
    if expected == actual {
        return None;
    }
    Some(format!("expected order [{}], got [{}]", expected.join(", "), actual.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response() -> Value {
        json!({"hits": {"hits": [
            {"_source": {"doc": "a", "specmatrix.run": "sm-1"}},
            {"_source": {"doc": "c", "specmatrix.run": "sm-1"}}
        ]}})
    }

    #[test]
    fn reads_the_marker_from_each_returned_row() {
        assert_eq!(
            returned_markers(&response(), "/hits/hits", "/_source/doc"),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    /// A row whose marker is missing is not counted, rather than counted as an
    /// empty string that would collide with another row.
    #[test]
    fn a_row_without_the_marker_is_skipped() {
        let response = json!({"hits": {"hits": [{"_source": {"other": 1}}]}});
        assert!(returned_markers(&response, "/hits/hits", "/_source/doc").is_empty());
    }

    #[test]
    fn a_missing_records_pointer_yields_no_rows() {
        assert!(returned_markers(&json!({"error": "no such index"}), "/hits/hits", "/_source/doc")
            .is_empty());
    }

    /// Order is not the assertion. Two backends returning the same rows in a
    /// different order have not disagreed about the query.
    #[test]
    fn the_same_rows_in_a_different_order_match() {
        assert_eq!(compare(&["a".into(), "c".into()], &["c".into(), "a".into()]), None);
    }

    /// The failure this shape exists for: a backend returns a row the reference
    /// does not, because a negation matched a record where the field is absent.
    #[test]
    fn an_extra_row_is_reported_with_its_marker() {
        let detail = compare(&["a".into()], &["a".into(), "b".into()]).expect("a difference");
        assert!(detail.contains("unexpected: b"), "{detail}");
    }

    #[test]
    fn a_missing_row_is_reported_with_its_marker() {
        let detail = compare(&["a".into(), "c".into()], &["a".into()]).expect("a difference");
        assert!(detail.contains("missing: c"), "{detail}");
    }

    #[test]
    fn both_directions_are_reported_together() {
        let detail = compare(&["a".into()], &["b".into()]).expect("a difference");
        assert!(detail.contains("missing: a") && detail.contains("unexpected: b"), "{detail}");
    }

    /// An empty result set is a legitimate expectation: a check asserting that
    /// a query matches nothing must be able to pass.
    #[test]
    fn expecting_nothing_and_getting_nothing_matches() {
        assert_eq!(compare(&[], &[]), None);
    }

    #[test]
    fn expecting_nothing_and_getting_something_is_a_difference() {
        assert!(compare(&[], &["a".into()]).is_some());
    }

    #[test]
    fn an_ordered_comparison_is_sensitive_to_order() {
        assert_eq!(compare_ordered(&["a".into(), "b".into()], &["a".into(), "b".into()]), None);
        let detail =
            compare_ordered(&["a".into(), "b".into()], &["b".into(), "a".into()]).expect("differs");
        assert!(detail.contains("expected order [a, b]"), "{detail}");
    }
}
