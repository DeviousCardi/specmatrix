//! Many backends, one page.
//!
//! Deliberately without a total, a percentage or an ordering. A backend that
//! fails ten cosmetic checks is not worse than one that silently drops
//! metrics, and any single number invites exactly that comparison. The page
//! publishes the cases and lets a reader weigh what matters to them.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::runner::{Outcome, Verdict};

#[derive(Debug, Serialize)]
pub struct Matrix {
    /// The date this was produced. A matrix without one is a claim about the
    /// past that reads as a claim about the present.
    pub generated: String,
    pub suite: String,
    pub outcomes: Vec<Outcome>,
}

impl Matrix {
    pub fn new(suite: &str, outcomes: Vec<Outcome>, generated: chrono::DateTime<chrono::Utc>) -> Self {
        Matrix {
            generated: generated.to_rfc3339(),
            suite: suite.to_string(),
            outcomes,
        }
    }

    /// Every check any column ran, so a column that skipped one is visibly
    /// blank rather than quietly missing from the table.
    pub fn check_ids(&self) -> Vec<&str> {
        let ids: BTreeSet<&str> = self
            .outcomes
            .iter()
            .flat_map(|o| o.results.iter().map(|r| r.id.as_str()))
            .collect();
        ids.into_iter().collect()
    }

    pub fn to_markdown(&self) -> String {
        let mut page = String::new();
        page.push_str(&format!("# {}\n\n", self.suite));
        page.push_str(&format!("Generated {}.\n\n", &self.generated[..10]));

        page.push_str("| Check |");
        for outcome in &self.outcomes {
            page.push_str(&format!(" {} |", column_heading(outcome)));
        }
        page.push_str("\n| --- |");
        for _ in &self.outcomes {
            page.push_str(" --- |");
        }
        page.push('\n');

        for id in self.check_ids() {
            page.push_str(&format!("| `{id}` |"));
            for outcome in &self.outcomes {
                let cell = match outcome.results.iter().find(|r| r.id == id) {
                    Some(result) => format!("{} — {}", label(result.verdict), escape(&result.detail)),
                    None => "not run".to_string(),
                };
                page.push_str(&format!(" {cell} |"));
            }
            page.push('\n');
        }

        page.push_str(
            "\n`N/A` means the backend could not be asked — an encoding it does not accept, a \
query endpoint it does not implement, or a control that did not pass. It is not a verdict about \
the backend and is never counted with the others.\n",
        );
        page
    }
}

fn column_heading(outcome: &Outcome) -> String {
    match (&outcome.backend_version, &outcome.backend_image) {
        // The reported version and the pinned tag can disagree, and a reader
        // needs to know which build the column describes. Show both when they
        // do not already say the same thing.
        (Some(version), Some(image)) if !image.contains(version.trim_start_matches('v')) => {
            format!("{} {} (image {})", outcome.backend, version, image)
        }
        (Some(version), _) => format!("{} {}", outcome.backend, version),
        (None, Some(image)) => format!("{} (image {})", outcome.backend, image),
        (None, None) => format!("{} (version unknown)", outcome.backend),
    }
}

/// A pipe inside a detail would end the table cell early.
fn escape(detail: &str) -> String {
    detail.replace('|', "\\|").replace('\n', " ")
}

fn label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "PASS",
        Verdict::Reject => "REJECT",
        Verdict::Alter => "ALTER",
        Verdict::NotApplicable => "N/A",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CheckResult;

    fn outcome(backend: &str, version: Option<&str>, image: Option<&str>,
               rows: Vec<(&str, Verdict, &str)>) -> Outcome {
        Outcome {
            backend: backend.to_string(),
            backend_version: version.map(str::to_string),
            backend_image: image.map(str::to_string),
            suite: "otlp-logs".to_string(),
            url: format!("http://localhost/{backend}"),
            results: rows
                .into_iter()
                .map(|(id, verdict, detail)| CheckResult {
                    id: id.to_string(),
                    title: String::new(),
                    verdict,
                    detail: detail.to_string(),
                })
                .collect(),
        }
    }

    fn two() -> Matrix {
        let when = chrono::DateTime::parse_from_rfc3339("2026-09-08T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        Matrix::new(
            "otlp-logs",
            vec![
                outcome("parseable", Some("2.9.4"), Some("quay.io/parseablehq/parseable:v2.9.4"), vec![
                    ("otlp-logs/minimal-record", Verdict::Pass, "200, round trip intact"),
                    ("otlp-logs/body-invalid-utf8", Verdict::Reject, "400 invalid unicode"),
                ]),
                outcome("loki", Some("release-3.1.x-89fe788"), Some("grafana/loki:3.1.1"), vec![
                    ("otlp-logs/minimal-record", Verdict::Pass, "204, round trip intact"),
                    ("otlp-logs/body-invalid-utf8", Verdict::Alter, "bytes replaced with U+FFFD"),
                ]),
            ],
            when,
        )
    }

    /// A matrix without versions is a claim about the past that reads as a
    /// claim about the present.
    #[test]
    fn every_column_carries_its_version() {
        let page = two().to_markdown();
        assert!(page.contains("parseable 2.9.4"), "{page}");
        assert!(page.contains("release-3.1.x-89fe788"), "{page}");
    }

    /// When the reported version does not match the pinned tag, both are
    /// shown: a reader needs to know which build the column describes.
    #[test]
    fn a_version_that_disagrees_with_the_image_shows_both() {
        let page = two().to_markdown();
        assert!(page.contains("image grafana/loki:3.1.1"), "{page}");
        // Parseable's reported version is in its tag, so the tag is not repeated.
        assert!(!page.contains("image quay.io/parseablehq/parseable:v2.9.4"), "{page}");
    }

    #[test]
    fn the_page_is_dated() {
        assert!(two().to_markdown().contains("2026-09-08"));
    }

    #[test]
    fn one_row_per_check_across_all_backends() {
        let page = two().to_markdown();
        assert_eq!(page.matches("otlp-logs/minimal-record").count(), 1);
        assert_eq!(page.matches("otlp-logs/body-invalid-utf8").count(), 1);
    }

    /// A check a backend never ran is not the same as one it passed.
    #[test]
    fn a_check_a_backend_never_ran_is_blank_not_a_pass() {
        let mut matrix = two();
        matrix.outcomes[1].results.remove(0);
        let page = matrix.to_markdown();
        let row = page
            .lines()
            .find(|l| l.contains("otlp-logs/minimal-record"))
            .expect("the row is present");
        assert!(row.contains("not run"), "{row}");
    }

    /// No score and no ranking. A backend failing ten cosmetic checks is not
    /// worse than one silently dropping metrics, and a single number invites
    /// exactly that comparison.
    #[test]
    fn the_page_carries_no_score_or_ranking() {
        let page = two().to_markdown().to_lowercase();
        for banned in ["score", "rank", "%", " out of ", "best", "worst", "winner"] {
            assert!(!page.contains(banned), "matrix must not contain {banned:?}");
        }
    }

    #[test]
    fn not_applicable_is_explained_on_the_page() {
        let page = two().to_markdown();
        assert!(page.contains("N/A"), "{page}");
        assert!(page.contains("never counted with the others"), "{page}");
    }

    /// A detail containing a pipe would end the table cell early and shift
    /// every column after it.
    #[test]
    fn a_pipe_in_a_detail_does_not_break_the_table() {
        let when = chrono::Utc::now();
        let matrix = Matrix::new("es-bulk", vec![outcome(
            "quickwit", Some("0.8.2"), Some("quickwit/quickwit:0.8.2"),
            vec![("es-bulk/x", Verdict::Alter, r#"query {a="b"}|c="d" disagreed"#)],
        )], when);
        let page = matrix.to_markdown();
        let row = page.lines().find(|l| l.contains("es-bulk/x")).unwrap();
        assert_eq!(row.matches('|').count() - row.matches("\\|").count(), 3, "{row}");
    }

    /// The JSON is the artefact the page is rendered from, so it has to carry
    /// everything the page shows.
    #[test]
    fn the_json_carries_versions_images_and_reasons() {
        let json = serde_json::to_value(two()).unwrap();
        assert_eq!(json["suite"], "otlp-logs");
        assert!(json["generated"].as_str().unwrap().starts_with("2026-09-08"));
        assert_eq!(json["outcomes"][0]["backend_version"], "2.9.4");
        assert_eq!(json["outcomes"][1]["backend_image"], "grafana/loki:3.1.1");
        assert_eq!(json["outcomes"][1]["results"][1]["verdict"], "alter");
    }
}
