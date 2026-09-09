//! Many backends, one page.
//!
//! Deliberately without a total, a percentage or an ordering. A backend that
//! fails ten cosmetic checks is not worse than one that silently drops
//! metrics, and any single number invites exactly that comparison. The page
//! publishes the cases and lets a reader weigh what matters to them.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::runner::{Outcome, Verdict};

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct Matrix {
    /// The date this was produced. A matrix without one is a claim about the
    /// past that reads as a claim about the present.
    pub generated: String,
    pub suite: String,
    pub outcomes: Vec<Outcome>,
}

impl Matrix {
    pub fn new(
        suite: &str,
        outcomes: Vec<Outcome>,
        generated: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Matrix { generated: generated.to_rfc3339(), suite: suite.to_string(), outcomes }
    }

    /// Every check any column ran, so a column that skipped one is visibly
    /// blank rather than quietly missing from the table.
    pub fn check_ids(&self) -> Vec<&str> {
        let ids: BTreeSet<&str> =
            self.outcomes.iter().flat_map(|o| o.results.iter().map(|r| r.id.as_str())).collect();
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
                    Some(result) => {
                        format!("{} — {}", label(result.verdict), escape(&result.detail))
                    }
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

/// A previous run of the same suite, for the page to compare against.
///
/// The strongest evidence this project can offer that a finding is acted on
/// is a cell changing from `ALTER` to `PASS` with the version it changed at
/// — which needs the previous verdict beside the new one, not just linked.
pub struct History<'a> {
    /// Where the previous run's own page lives, relative to the page being
    /// rendered now.
    pub link: String,
    pub matrix: &'a Matrix,
}

impl Matrix {
    /// A single static page, rendered from the JSON beside it.
    ///
    /// `corpus_commit` identifies the corpus the run used, so a reader can
    /// check out exactly the checks that produced these cells. `history`, when
    /// given, is the most recent prior run of this same suite: a cell whose
    /// verdict changed says so and links back to it.
    pub fn to_html(&self, corpus_commit: Option<&str>, history: Option<&History>) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{} — SpecMatrix</title>\n<style>{}</style>\n</head><body>\n",
            escape_html(&self.suite),
            STYLE
        ));
        out.push_str(&format!("<h1>{}</h1>\n", escape_html(&self.suite)));
        out.push_str(&format!(
            "<p class=\"meta\">Generated {}. Rendered from <code>matrix.json</code> beside this page; every cell in it was produced by a run recorded there.</p>\n",
            escape_html(&self.generated[..10])
        ));
        if let Some(commit) = corpus_commit {
            out.push_str(&format!(
                "<p class=\"meta\">Corpus at commit <code>{}</code>.</p>\n",
                escape_html(commit)
            ));
        }
        if let Some(h) = history {
            out.push_str(&format!(
                "<p class=\"meta\">Compared against the <a href=\"{}\">run of {}</a>; a cell that changed says so.</p>\n",
                escape_html(&h.link),
                escape_html(&h.matrix.generated[..10])
            ));
        }

        out.push_str("<table>\n<thead><tr><th>Check</th>");
        for outcome in &self.outcomes {
            out.push_str(&format!(
                "<th>{}<span class=\"ver\">{}</span></th>",
                escape_html(&outcome.backend),
                escape_html(&version_line(outcome))
            ));
        }
        out.push_str("</tr></thead>\n<tbody>\n");

        for id in self.check_ids() {
            out.push_str(&format!(
                "<tr><th class=\"check\"><a href=\"../../../cases/{}.yaml\"><code>{}</code></a></th>",
                escape_html(id),
                escape_html(id)
            ));
            for outcome in &self.outcomes {
                match outcome.results.iter().find(|r| r.id == id) {
                    Some(result) => {
                        let class = match result.verdict {
                            Verdict::Pass => "pass",
                            Verdict::Reject => "reject",
                            Verdict::Alter => "alter",
                            Verdict::NotApplicable => "na",
                        };
                        let hist = history.and_then(|h| {
                            let prev_outcome =
                                h.matrix.outcomes.iter().find(|o| o.backend == outcome.backend)?;
                            let prev = prev_outcome.results.iter().find(|r| r.id == id)?;
                            if prev.verdict == result.verdict {
                                return None;
                            }
                            Some(format!(
                                "<a class=\"hist\" href=\"{}\">changed from {} on {}</a>",
                                escape_html(&h.link),
                                label(prev.verdict),
                                escape_html(&h.matrix.generated[..10])
                            ))
                        });
                        out.push_str(&format!(
                            "<td class=\"{class}\"><span class=\"v\">{}</span><span class=\"d\">{}</span>{}</td>",
                            label(result.verdict),
                            escape_html(&result.detail),
                            hist.unwrap_or_default()
                        ));
                    }
                    None => {
                        out.push_str("<td class=\"notrun\"><span class=\"v\">not run</span></td>")
                    }
                }
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody></table>\n");
        out.push_str(
            "<p class=\"meta\"><strong>N/A</strong> means the backend could not be asked — an encoding it does not accept, a query endpoint it does not implement, or a control that did not pass. It is not a verdict about the backend and is never counted with the others.</p>\n<p class=\"meta\"><strong>ALTER</strong> means the write was accepted and what came back was different, or never came back at all. Nothing errored at the time.</p>\n",
        );
        out.push_str("</body></html>\n");
        out
    }
}

/// N/A is given a colour that resembles neither a pass nor a failure, because
/// it is not a verdict about the backend.
const STYLE: &str = "\
body{font:15px/1.5 system-ui,sans-serif;margin:2rem auto;max-width:none;padding:0 1.5rem;color:#111}\
h1{font-size:1.4rem;margin:0 0 .5rem}\
.meta{color:#555;max-width:60rem}\
table{border-collapse:collapse;margin:1.5rem 0;font-size:13px}\
th,td{border:1px solid #ddd;padding:.4rem .6rem;text-align:left;vertical-align:top}\
thead th{background:#f6f6f6;white-space:nowrap}\
.ver{display:block;font-weight:400;color:#666;font-size:11px}\
th.check{font-weight:400;white-space:nowrap}\
td{max-width:22rem}\
.v{display:block;font-weight:600;font-size:11px;letter-spacing:.04em}\
.d{display:block;color:#444;font-size:11px;word-break:break-word}\
.pass{background:#f3faf3}.pass .v{color:#216c2a}\
.reject{background:#fdf6ee}.reject .v{color:#8a5a12}\
.alter{background:#fdf0f0}.alter .v{color:#a01b1b}\
.na{background:#f4f4f7}.na .v{color:#5a5a70}\
.notrun{background:#fff}.notrun .v{color:#999}\
.hist{display:block;font-size:10px;color:#555;margin-top:.2rem;text-decoration:underline}\
";

fn version_line(outcome: &Outcome) -> String {
    match (&outcome.backend_version, &outcome.backend_image) {
        (Some(v), Some(i)) if !i.contains(v.trim_start_matches('v')) => format!("{v} · {i}"),
        (Some(v), _) => v.clone(),
        (None, Some(i)) => format!("image {i}"),
        (None, None) => "version unknown".to_string(),
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
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

    fn outcome(
        backend: &str,
        version: Option<&str>,
        image: Option<&str>,
        rows: Vec<(&str, Verdict, &str)>,
    ) -> Outcome {
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
                outcome(
                    "parseable",
                    Some("2.9.4"),
                    Some("quay.io/parseablehq/parseable:v2.9.4"),
                    vec![
                        ("otlp-logs/minimal-record", Verdict::Pass, "200, round trip intact"),
                        ("otlp-logs/body-invalid-utf8", Verdict::Reject, "400 invalid unicode"),
                    ],
                ),
                outcome(
                    "loki",
                    Some("release-3.1.x-89fe788"),
                    Some("grafana/loki:3.1.1"),
                    vec![
                        ("otlp-logs/minimal-record", Verdict::Pass, "204, round trip intact"),
                        (
                            "otlp-logs/body-invalid-utf8",
                            Verdict::Alter,
                            "bytes replaced with U+FFFD",
                        ),
                    ],
                ),
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
        let matrix = Matrix::new(
            "es-bulk",
            vec![outcome(
                "quickwit",
                Some("0.8.2"),
                Some("quickwit/quickwit:0.8.2"),
                vec![("es-bulk/x", Verdict::Alter, r#"query {a="b"}|c="d" disagreed"#)],
            )],
            when,
        );
        let page = matrix.to_markdown();
        let row = page.lines().find(|l| l.contains("es-bulk/x")).unwrap();
        assert_eq!(row.matches('|').count() - row.matches("\\|").count(), 3, "{row}");
    }

    /// The page must never carry a score, a percentage or an ordering.
    #[test]
    fn the_html_page_carries_no_score_or_ranking() {
        let page = two().to_html(None, None).to_lowercase();
        for banned in ["score", "rank", "%", " out of ", "best", "worst", "winner"] {
            assert!(!page.contains(banned), "page must not contain {banned:?}");
        }
    }

    #[test]
    fn the_html_page_is_dated_and_versioned() {
        let page = two().to_html(Some("abc1234"), None);
        assert!(page.contains("2026-09-08"), "{page}");
        assert!(page.contains("2.9.4"), "{page}");
        assert!(page.contains("abc1234"), "{page}");
    }

    /// N/A must be visually distinct from a pass and from a failure, because
    /// it is not a verdict about the backend.
    #[test]
    fn each_verdict_gets_its_own_class() {
        let page = two().to_html(None, None);
        for class in ["class=\"pass\"", "class=\"reject\"", "class=\"alter\""] {
            assert!(page.contains(class), "missing {class}");
        }
        assert!(page.contains(".na{background"), "N/A needs a colour of its own");
        assert!(page.contains("never counted with the others"), "{page}");
    }

    /// A detail carrying markup must not be able to close a tag.
    #[test]
    fn a_detail_containing_markup_is_escaped() {
        let when = chrono::Utc::now();
        let matrix = Matrix::new(
            "es-bulk",
            vec![outcome(
                "stub",
                Some("1"),
                Some("stub:1"),
                vec![("es-bulk/x", Verdict::Alter, r#"<script>alert("x")</script> & "quoted""#)],
            )],
            when,
        );
        let page = matrix.to_html(None, None);
        assert!(!page.contains("<script>"), "markup must be escaped");
        assert!(page.contains("&lt;script&gt;"), "{page}");
        assert!(page.contains("&amp;"), "{page}");
    }

    /// A cell whose verdict differs from the same check on the same backend
    /// in the previous run says so, with a link — the strongest evidence a
    /// finding was acted on is a cell moving from ALTER to PASS with the
    /// version it changed at.
    #[test]
    fn a_cell_that_changed_since_the_previous_run_says_so() {
        let earlier = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let previous = Matrix::new(
            "otlp-logs",
            vec![outcome(
                "loki",
                Some("3.1.1"),
                None,
                vec![("otlp-logs/x", Verdict::Alter, "was altered")],
            )],
            earlier,
        );
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let current = Matrix::new(
            "otlp-logs",
            vec![outcome(
                "loki",
                Some("3.7.7"),
                None,
                vec![("otlp-logs/x", Verdict::Pass, "fixed")],
            )],
            now,
        );
        let history = History {
            link: "../../2026-06-01/otlp-logs/matrix.html".to_string(),
            matrix: &previous,
        };
        let page = current.to_html(None, Some(&history));
        assert!(page.contains("changed from ALTER"), "{page}");
        assert!(page.contains("2026-06-01/otlp-logs/matrix.html"), "{page}");
    }

    /// A check with no prior verdict — new this run, or the previous run
    /// simply did not carry that backend — gets no history annotation. There
    /// is nothing to compare it against.
    #[test]
    fn a_new_check_carries_no_history_annotation() {
        let earlier = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let previous = Matrix::new("otlp-logs", vec![], earlier);
        let page = two().to_html(
            None,
            Some(&History { link: "elsewhere.html".to_string(), matrix: &previous }),
        );
        assert!(!page.contains("class=\"hist\""), "{page}");
    }

    /// Each row links to the check that produced it, so a reader can see the
    /// rule the verdict rests on.
    #[test]
    fn each_row_links_to_its_case_file() {
        let page = two().to_html(None, None);
        assert!(page.contains("cases/otlp-logs/minimal-record.yaml"), "{page}");
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
