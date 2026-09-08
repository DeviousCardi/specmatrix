//! Sends each case, reads it back, and decides a verdict.
//!
//! The interesting part is not sending. It is deciding, once a backend has
//! accepted a write, whether what comes back is the same thing. A backend that
//! refuses a payload tells you so; one that alters it does not.

use anyhow::{Context, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

use crate::backend::{Auth, Backend, Readback, Request};
use crate::case::Case;
use crate::template::{self, Vars};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Written and read back unchanged.
    Pass,
    /// Refused at ingest, with an error.
    Reject,
    /// Accepted, then absent or different when read back.
    Alter,
    /// The backend could not be asked. Not a verdict about the backend, and
    /// never counted towards a pass or a failure.
    #[serde(rename = "n/a")]
    NotApplicable,
}

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub id: String,
    pub title: String,
    pub verdict: Verdict,
    /// One line saying what happened. Shown beside the verdict.
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Outcome {
    pub backend: String,
    pub backend_version: Option<String>,
    pub suite: String,
    pub url: String,
    pub results: Vec<CheckResult>,
}

impl Outcome {
    pub fn count(&self, verdict: Verdict) -> usize {
        self.results.iter().filter(|r| r.verdict == verdict).count()
    }
}

pub struct Runner {
    backend: Backend,
    base_url: String,
    client: reqwest::blocking::Client,
    verbose: bool,
}

impl Runner {
    pub fn new(backend: Backend, base_url: String, verbose: bool) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { backend, base_url: base_url.trim_end_matches('/').to_string(), client, verbose })
    }

    pub fn run_suite(&self, suite: &str, cases: &[Case]) -> Outcome {
        let backend_version = self.backend_version().ok().flatten();
        let mut results = Vec::new();

        // Controls run first. If one does not pass, the rest of the suite
        // cannot say anything about the backend, and running it anyway would
        // print one fact several times over.
        let (controls, rest): (Vec<&Case>, Vec<&Case>) =
            cases.iter().partition(|case| case.control);

        let mut blocked: Option<String> = None;
        for case in &controls {
            let result = self.run_or_report(suite, case);
            if blocked.is_none() {
                blocked = match result.verdict {
                    Verdict::Pass => None,
                    Verdict::Reject => Some(format!("control rejected: {}", result.detail)),
                    Verdict::Alter => Some(
                        "control altered: adapter mapping is wrong, fix it before trusting any row"
                            .to_string(),
                    ),
                    // The control row already carries the full reason. Repeating
                    // it on every remaining row turns one fact into five.
                    Verdict::NotApplicable if result.detail.starts_with("harness error") => {
                        Some(format!("not attempted: control {} did not run", case.id))
                    }
                    Verdict::NotApplicable => Some(result.detail.clone()),
                };
            }
            results.push(result);
        }

        for case in &rest {
            results.push(match &blocked {
                Some(reason) => CheckResult {
                    id: case.id.clone(),
                    title: case.title.clone(),
                    verdict: Verdict::NotApplicable,
                    detail: reason.clone(),
                },
                None => self.run_or_report(suite, case),
            });
        }
        results.sort_by(|a, b| a.id.cmp(&b.id));
        Outcome {
            backend: self.backend.name.clone(),
            backend_version,
            suite: suite.to_string(),
            url: self.base_url.clone(),
            results,
        }
    }

    /// Runs one case, turning a harness failure into a row rather than losing
    /// the whole suite. A harness error is `N/A`, never a verdict: the runner
    /// or the network failed, and the backend has not been asked anything.
    fn run_or_report(&self, suite: &str, case: &Case) -> CheckResult {
        match self.run_case(suite, case) {
            Ok(result) => result,
            Err(e) => CheckResult {
                id: case.id.clone(),
                title: case.title.clone(),
                verdict: Verdict::NotApplicable,
                detail: format!("harness error: {e:#}"),
            },
        }
    }

    fn run_case(&self, suite: &str, case: &Case) -> Result<CheckResult> {
        let vars = self.vars_for(case);
        let protocol = self
            .backend
            .protocols
            .get(suite)
            .with_context(|| format!("adapter {} has no protocol {suite}", self.backend.name))?;

        // A backend that does not speak this encoding is not failing the check,
        // it is ineligible for it. Sending anyway would manufacture one
        // rejection per case out of a single fact.
        if !protocol.formats.is_empty() && !protocol.formats.contains(&case.send.format) {
            return Ok(self.result(
                case,
                Verdict::NotApplicable,
                format!("encoding {} not accepted by this backend", case.send.format),
            ));
        }

        let payload_path = case.payload_path();
        let raw = std::fs::read(&payload_path)
            .with_context(|| format!("reading payload {}", payload_path.display()))?;
        let body = template::render_bytes(&raw, &vars);

        let (status, response) = self.send(&protocol.ingest, &vars, body)?;
        let accepted = (200..300).contains(&status);
        let expects_rejection = case.expect.ingest == "rejected";

        if !accepted {
            let detail = format!("{status} {}", first_line(&response));
            return Ok(self.result(
                case,
                if expects_rejection { Verdict::Pass } else { Verdict::Reject },
                detail,
            ));
        }
        if expects_rejection {
            return Ok(self.result(
                case,
                Verdict::Alter,
                "accepted a payload the check expects to be refused".into(),
            ));
        }

        let Some(expect) = case.expect.readback.as_ref() else {
            return Ok(self.result(case, Verdict::Pass, format!("{status}, no read-back declared")));
        };
        let Some(readback) = protocol.readback.as_ref() else {
            return Ok(self.result(
                case,
                Verdict::Pass,
                format!("{status}, adapter declares no read-back"),
            ));
        };

        match self.read_back(readback, &vars)? {
            None => Ok(self.result(
                case,
                Verdict::Alter,
                format!("accepted ({status}) but never became queryable"),
            )),
            Some(record) => {
                let rendered = template::render_bytes(&std::fs::read(&payload_path)?, &vars);
                let (sent, sent_was_lossy) = parse_sent(&rendered);

                let observed: Vec<String> = expect
                    .on
                    .iter()
                    .map(|field| {
                        let want = crate::otlp::logical_field(&sent, field);
                        let got = self.field_of(readback, &record, field);
                        (field, want, got)
                    })
                    .filter(|(_, want, got)| expect.match_ != "exact" || want != got)
                    .map(|(field, want, got)| {
                        format!(
                            "{field}: sent {}, read back {}",
                            render_opt(&want),
                            render_opt(&got)
                        )
                    })
                    .collect();

                // A payload that is not valid UTF-8 cannot be compared field by
                // field: our own expectation had to be produced by a lossy
                // decode, so "equal" only means the backend replaced the same
                // bytes we did. Report the substitution rather than a match.
                if sent_was_lossy && expect.match_ == "exact" {
                    let replaced: Vec<String> = expect
                        .on
                        .iter()
                        .map(|field| {
                            let want = crate::otlp::logical_field(&sent, field);
                            let got = self.field_of(readback, &record, field);
                            if want == got {
                                format!("{field}: invalid bytes replaced with U+FFFD")
                            } else {
                                format!(
                                    "{field}: sent {}, read back {}",
                                    render_opt(&want),
                                    render_opt(&got)
                                )
                            }
                        })
                        .collect();
                    return Ok(self.result(case, Verdict::Alter, replaced.join("; ")));
                }

                match expect.match_.as_str() {
                    "exact" if observed.is_empty() => {
                        Ok(self.result(case, Verdict::Pass, format!("{status}, round trip intact")))
                    }
                    "exact" => Ok(self.result(case, Verdict::Alter, observed.join("; "))),
                    // `present` is for values the project has not adjudicated.
                    // Show what the backend stored so the divergence is visible,
                    // but do not call it a failure on the strength of a guess.
                    "present" => {
                        Ok(self.result(case, Verdict::Pass, format!("recorded — {}", observed.join("; "))))
                    }
                    other => anyhow::bail!(
                        "case {} declares readback.match: {other:?}; expected `exact` or `present`",
                        case.id
                    ),
                }
            }
        }
    }

    fn result(&self, case: &Case, verdict: Verdict, detail: String) -> CheckResult {
        CheckResult { id: case.id.clone(), title: case.title.clone(), verdict, detail }
    }

    /// Variables available to payloads and adapter templates.
    fn vars_for(&self, case: &Case) -> Vars {
        let run_key = format!(
            "sm-{:x}",
            rand::random::<u64>()
        );
        // One stream per case keeps a failure in one check from contaminating
        // the next, and makes teardown a single call.
        let stream = format!(
            "specmatrix_{}",
            case.id.replace(['/', '-', '.'], "_").to_lowercase()
        );
        let now = chrono::Utc::now();
        let mut vars = Vars::new();
        vars.insert("run_key", run_key);
        vars.insert("suite_stream", stream);
        vars.insert("window_start", (now - chrono::Duration::minutes(5)).to_rfc3339());
        vars.insert("window_end", (now + chrono::Duration::minutes(5)).to_rfc3339());
        vars
    }

    fn send(&self, req: &Request, vars: &Vars, body: Vec<u8>) -> Result<(u16, String)> {
        let (method, path) = req.parts()?;
        let url = format!("{}{}", self.base_url, template::render(&path, vars));
        let mut builder = match method.as_str() {
            "POST" => self.client.post(&url),
            "PUT" => self.client.put(&url),
            "GET" => self.client.get(&url),
            "DELETE" => self.client.delete(&url),
            other => anyhow::bail!("unsupported method {other}"),
        };
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), template::render(v, vars));
        }
        builder = self.authenticate(builder);
        if self.verbose {
            eprintln!("--> {method} {url} ({} bytes)", body.len());
        }
        let response = builder.body(body).send()?;
        let status = response.status().as_u16();
        let text = response.text().unwrap_or_default();
        if self.verbose {
            eprintln!("<-- {status} {}", first_line(&text));
        }
        Ok((status, text))
    }

    /// Polls until the record appears or the adapter's timeout elapses.
    ///
    /// Most backends acknowledge a write before it is queryable, so a single
    /// immediate read would report every backend as dropping data.
    fn read_back(&self, readback: &Readback, vars: &Vars) -> Result<Option<serde_json::Value>> {
        let run_key = vars.get("run_key").cloned().unwrap_or_default();
        let deadline = Instant::now() + Duration::from_millis(readback.poll.timeout_ms);
        loop {
            let (method, path) = readback.request.parts()?;
            let url = format!("{}{}", self.base_url, template::render(&path, vars));
            let mut builder = match method.as_str() {
                "POST" => self.client.post(&url),
                _ => self.client.get(&url),
            };
            for (k, v) in &readback.request.headers {
                builder = builder.header(k.as_str(), template::render(v, vars));
            }
            builder = self.authenticate(builder);
            if let Some(body) = &readback.request.body {
                let rendered = template::render(&body.to_string(), vars);
                builder = builder
                    .header("Content-Type", "application/json")
                    .body(rendered);
            }
            if let Ok(response) = builder.send() {
                let text = response.text().unwrap_or_default();
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(record) = first_record(&value, &readback.records, &run_key) {
                        return Ok(Some(record));
                    }
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(readback.poll.interval_ms));
        }
    }

    /// Looks a logical field up in a backend record, honouring the adapter's
    /// renames. Renaming is allowed; there is deliberately nowhere here to
    /// rewrite a value.
    fn field_of(
        &self,
        readback: &Readback,
        record: &serde_json::Value,
        field: &str,
    ) -> Option<serde_json::Value> {
        if let Some(pointer) = readback.fields.get(field) {
            return record.pointer(pointer).cloned();
        }
        record.get(field).cloned()
    }

    fn authenticate(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        match self.backend.auth.as_ref() {
            Some(Auth { kind, username, password, .. }) if kind == "basic" => {
                builder.basic_auth(username.clone().unwrap_or_default(), password.clone())
            }
            Some(Auth { kind, token, .. }) if kind == "bearer" => {
                builder.bearer_auth(token.clone().unwrap_or_default())
            }
            _ => builder,
        }
    }

    fn backend_version(&self) -> Result<Option<String>> {
        let Some(vf) = self.backend.version_from.as_ref() else {
            return Ok(None);
        };
        let req = Request { request: vf.request.clone(), headers: Default::default(), body: None };
        let (status, text) = self.send(&req, &Vars::new(), Vec::new())?;
        if !(200..300).contains(&status) {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        // A version may sit at the top level or be nested. Accept both, as the
        // adapter's `field` is documented to allow either.
        let found = if vf.field.starts_with('/') {
            value.pointer(&vf.field)
        } else {
            value.get(&vf.field)
        };
        Ok(found.and_then(|v| v.as_str()).map(str::to_string))
    }
}

/// Pulls this run's record out of a read-back response.
///
/// Matching on the run key rather than taking the first element rules out two
/// false positives: an error response that happens to be a JSON object, and a
/// record left behind by an earlier run.
fn first_record(
    value: &serde_json::Value,
    records_pointer: &str,
    run_key: &str,
) -> Option<serde_json::Value> {
    let node = if records_pointer.is_empty() {
        value
    } else {
        value.pointer(records_pointer)?
    };
    let items: Vec<&serde_json::Value> = match node {
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(_) => vec![node],
        _ => return None,
    };
    items
        .into_iter()
        .find(|record| record.to_string().contains(run_key))
        .cloned()
}

/// Parses the payload we sent so its fields can be compared with what came
/// back. Returns whether a lossy decode was needed: a payload carrying invalid
/// UTF-8 is deliberate in this corpus, and a strict parse would fail and make
/// every field read as absent.
fn parse_sent(bytes: &[u8]) -> (serde_json::Value, bool) {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => (value, false),
        Err(_) => {
            let lossy = String::from_utf8_lossy(bytes);
            let value = serde_json::from_str(&lossy).unwrap_or(serde_json::Value::Null);
            (value, true)
        }
    }
}

fn render_opt(value: &Option<serde_json::Value>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "<absent>".to_string(),
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.len() > 120 { format!("{}…", &line[..120]) } else { line.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A leftover record from an earlier run must not be mistaken for this
    /// one's, or a backend that dropped the write would look like it kept it.
    #[test]
    fn first_record_matches_on_the_run_key() {
        let response = json!({
            "hits": [
                {"body": "from an earlier run", "specmatrix.run": "sm-old"},
                {"body": "ours", "specmatrix.run": "sm-new"}
            ]
        });
        let found = first_record(&response, "/hits", "sm-new").expect("record present");
        assert_eq!(found.get("body").unwrap(), "ours");
    }

    #[test]
    fn first_record_ignores_records_without_the_key() {
        let response = json!({"hits": [{"body": "someone else's"}]});
        assert!(first_record(&response, "/hits", "sm-new").is_none());
    }

    /// A backend that does not speak the case's encoding must be reported as
    /// ineligible without a request being sent. The URL here is a closed port:
    /// if the runner tried to reach it the test would fail with a harness
    /// error rather than N/A.
    #[test]
    fn unaccepted_encoding_yields_not_applicable_without_a_request() {
        let adapter: crate::backend::Backend = serde_yaml::from_str(
            r#"
name: protobuf-only
protocols:
  otlp-logs:
    formats: [otlp-protobuf]
    ingest:
      request: POST /v1/logs
"#,
        )
        .expect("adapter parses");

        let case: Case = serde_yaml::from_str(
            r#"
id: otlp-logs/minimal-record
protocol: otlp-logs
title: control
send:
  format: otlp-json
  body: cases/otlp-logs/minimal-record.json
expect:
  ingest: accepted
"#,
        )
        .expect("case parses");

        let runner = Runner::new(adapter, "http://127.0.0.1:1".into(), false).unwrap();
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");

        assert_eq!(result.verdict, Verdict::NotApplicable);
        assert!(result.detail.contains("otlp-json"), "detail: {}", result.detail);
    }

    #[test]
    fn parse_sent_reports_valid_utf8_as_strict() {
        let (value, lossy) = parse_sent(br#"{"a": "plain"}"#);
        assert!(!lossy);
        assert_eq!(value.get("a").unwrap(), "plain");
    }

    /// The corpus deliberately carries payloads that are not valid UTF-8. A
    /// strict parse fails on those, which would make every field read as absent
    /// and hide what the backend actually did.
    #[test]
    fn parse_sent_falls_back_to_a_lossy_decode() {
        let mut bytes = br#"{"a": "before-"#.to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(br#"-after"}"#);

        let (value, lossy) = parse_sent(&bytes);
        assert!(lossy, "invalid UTF-8 must be reported as a lossy decode");
        let a = value.get("a").and_then(|v| v.as_str()).unwrap_or_default();
        assert!(a.starts_with("before-") && a.ends_with("-after"));
        assert!(a.contains('\u{fffd}'), "replacement character expected, got {a:?}");
    }

    /// An error body is often a JSON object. Without the run key it would count
    /// as the record having arrived.
    #[test]
    fn first_record_rejects_an_error_object() {
        let response = json!({"error": "index not found"});
        assert!(first_record(&response, "", "sm-new").is_none());
    }
}
