//! Sends each case, reads it back, and decides a verdict.
//!
//! The interesting part is not sending. It is deciding, once a backend has
//! accepted a write, whether what comes back is the same thing. A backend that
//! refuses a payload tells you so; one that alters it does not.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
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

/// One HTTP response, kept as bytes so a body can be read in whatever encoding
/// the store actually used rather than the one it claimed.
struct Response {
    status: u16,
    content_type: Option<String>,
    body: Vec<u8>,
}

impl Response {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
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

        // A backend that speaks no encoding this case can be sent in is not
        // failing the check, it is ineligible for it. Sending anyway would
        // manufacture one rejection per case out of a single fact, which is
        // what 0.1 did to Quickwit. Eligibility is decided on what the case can
        // be *converted* to, not only on the format its file happens to use.
        let offered = case.send.encodings();
        let Some(encoding) = crate::encode::choose_encoding(&protocol.formats, &offered) else {
            return Ok(self.result(
                case,
                Verdict::NotApplicable,
                format!("encoding {} not accepted by this backend", offered.join(" or ")),
            ));
        };

        let payload_path = case.payload_path();
        let raw = std::fs::read(&payload_path)
            .with_context(|| format!("reading payload {}", payload_path.display()))?;
        let rendered = template::render_bytes(&raw, &vars);
        // A payload that cannot be carried by the chosen encoding is another
        // ineligibility, not a verdict: `body-invalid-utf8` cannot exist as a
        // protobuf string, and reporting that as a failure would be a claim
        // about the backend based on a limit of the wire format.
        let body = match crate::encode::to_wire(&case.send.format, &encoding, &rendered) {
            Ok(body) => body,
            Err(e) => {
                return Ok(self.result(
                    case,
                    Verdict::NotApplicable,
                    format!("payload cannot be encoded as {encoding}: {e}"),
                ))
            }
        };

        // Clear this case's stream before writing to it. The adapter has
        // carried a teardown request since 0.1 and nothing ever sent it, so a
        // rerun tested whatever the previous run left behind — which a query
        // check cannot tolerate, since it asserts on exactly which rows come
        // back. Sent before rather than after: a run that crashes leaves state,
        // and an "after" teardown is precisely the one that did not run.
        if let Some(teardown) = self.backend.teardown.as_ref() {
            let _ = self.send_declared(teardown, &vars);
        }
        // Then recreate whatever the store needs before it will accept a write.
        // A store that will not create an index on write has to be given one,
        // and the shape of that index belongs to the adapter: putting it in the
        // corpus would make a shared case carry one backend's schema.
        if let Some(setup) = self.backend.setup.as_ref() {
            let response = self.send_declared(setup, &vars)?;
            if !(200..300).contains(&response.status) && response.status != 400 {
                // 400 is tolerated because several stores answer it for "already
                // exists", which is not a failure to set up.
                return Ok(self.result(
                    case,
                    Verdict::NotApplicable,
                    format!("setup failed: {} {}", response.status, first_line(&response.text())),
                ));
            }
        }

        let response = self.send(&protocol.ingest, &vars, body)?;
        let status = response.status;
        let accepted = (200..300).contains(&status);

        if let Some(verdict) = ingest_verdict(&case.expect.ingest, accepted)
            .with_context(|| format!("case {}", case.id))?
        {
            let detail = if accepted {
                "accepted a payload the check expects to be refused".to_string()
            } else {
                format!("{status} {}", first_line(&response.text()))
            };
            return Ok(self.result(case, verdict, detail));
        }

        // A 2xx does not mean the data was kept. OTLP gives a store a way to
        // say it dropped part of a batch, and whether a store uses it is the
        // difference between a loud failure and a silent one — which is the
        // distinction this project exists to draw.
        let reported = if case.protocol.starts_with("otlp") {
            crate::otlp::export_report(&encoding, response.content_type.as_deref(), &response.body)
        } else {
            None
        };
        let reported = describe_report(reported.as_ref());

        if let Some(expect_query) = case.expect.query.as_ref() {
            let Some(query) = protocol.query.as_ref() else {
                return Ok(self.result(
                    case,
                    Verdict::NotApplicable,
                    "adapter declares no query endpoint for this protocol".into(),
                ));
            };
            let returned = self.run_query(case, query, expect_query, &vars)?;
            let difference = match expect_query.order.as_str() {
                "any" => crate::query::compare(&expect_query.returns, &returned),
                "as-listed" => crate::query::compare_ordered(&expect_query.returns, &returned),
                other => anyhow::bail!(
                    "case {} declares query.order: {other:?}; expected `any` or `as-listed`",
                    case.id
                ),
            };
            return Ok(match difference {
                None => self.result(case, Verdict::Pass, format!("{status}, query agrees")),
                // The data is intact and the same query answers differently.
                // Nothing errored and a dashboard rendered, which is what makes
                // this class worth a tool.
                Some(detail) => self.result(case, Verdict::Alter, detail),
            });
        }

        let Some(expect) = case.expect.readback.as_ref() else {
            return Ok(self.result(case, Verdict::Pass, format!("{status}, no read-back declared{reported}")));
        };
        let Some(readback) = protocol.readback.as_ref() else {
            return Ok(self.result(
                case,
                Verdict::Pass,
                format!("{status}, adapter declares no read-back{reported}"),
            ));
        };

        match self.read_back(readback, &vars)? {
            None => Ok(self.result(
                case,
                Verdict::Alter,
                format!("accepted ({status}) but never became queryable{reported}"),
            )),
            Some(record) => {
                let (sent, sent_was_lossy) = parse_sent(&rendered);

                // Every field the check names, read once, with the kind it
                // declared. Reading a value is not rewriting it: the kind
                // decides how two stored values are compared and what the
                // result line says, never what the backend stored.
                let mut readings = Vec::new();
                for spec in &expect.on {
                    let kind = match spec.kind_name() {
                        Some(name) => crate::compare::kind_from_str(name)
                            .with_context(|| format!("case {} field {}", case.id, spec.field()))?,
                        None => crate::compare::Kind::Raw,
                    };
                    let field = spec.field();
                    readings.push((
                        field.to_string(),
                        kind,
                        logical_field_for(&case.protocol, &sent, field),
                        self.field_of(readback, &record, field),
                    ));
                }

                let observed: Vec<String> = readings
                    .iter()
                    .filter(|(_, kind, want, got)| {
                        expect.match_ != "exact" || !crate::compare::equal(want, got, *kind)
                    })
                    .map(|(field, kind, want, got)| {
                        format!(
                            "{field}: sent {}, read back {}",
                            crate::compare::describe(want, *kind),
                            crate::compare::describe(got, *kind)
                        )
                    })
                    .collect();

                // A payload that is not valid UTF-8 cannot be compared field by
                // field: our own expectation had to be produced by a lossy
                // decode, so "equal" only means the backend replaced the same
                // bytes we did. Report the substitution rather than a match.
                if sent_was_lossy && expect.match_ == "exact" {
                    let replaced: Vec<String> = readings
                        .iter()
                        .map(|(field, kind, want, got)| {
                            if crate::compare::equal(want, got, *kind) {
                                format!("{field}: invalid bytes replaced with U+FFFD")
                            } else {
                                format!(
                                    "{field}: sent {}, read back {}",
                                    crate::compare::describe(want, *kind),
                                    crate::compare::describe(got, *kind)
                                )
                            }
                        })
                        .collect();
                    return Ok(self.result(case, Verdict::Alter, replaced.join("; ")));
                }

                match expect.match_.as_str() {
                    "exact" if observed.is_empty() => {
                        Ok(self.result(case, Verdict::Pass, format!("{status}, round trip intact{reported}")))
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
        let mut vars = Vars::new();
        vars.insert("run_key", run_key);
        vars.insert("suite_stream", stream);
        if let Some(field) = self.backend.run_key_field.clone() {
            vars.insert("run_key_field", field);
        }
        for (key, value) in time_vars(chrono::Utc::now()) {
            vars.insert(key, value);
        }
        vars
    }

    fn send(&self, req: &Request, vars: &Vars, body: Vec<u8>) -> Result<Response> {
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
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // Bytes, not text. A store may answer a JSON export with a protobuf
        // body, and a lossy string conversion destroys it before it can be
        // read — which is how that behaviour stayed invisible in 0.1.
        let body = response.bytes().map(|b| b.to_vec()).unwrap_or_default();
        let out = Response { status, content_type, body };
        if self.verbose {
            eprintln!("<-- {status} {}", first_line(&out.text()));
        }
        Ok(out)
    }

    /// Runs a query check's body and returns the marker of every row that came
    /// back.
    ///
    /// Polls for the same reason read-back does: a write is usually
    /// acknowledged before it is searchable. It settles once the row count
    /// stops changing, rather than once it reaches the expected count — a check
    /// that expects fewer rows than the backend will return has to see the
    /// extra ones, and that is the divergence such a check is looking for.
    fn run_query(
        &self,
        case: &Case,
        query: &crate::backend::Query,
        expect: &crate::case::QueryExpect,
        vars: &Vars,
    ) -> Result<Vec<String>> {
        let path = if expect.body.exists() {
            expect.body.clone()
        } else {
            case.dir.join(expect.body.file_name().unwrap_or_default())
        };
        let raw = std::fs::read(&path)
            .with_context(|| format!("reading query body {}", path.display()))?;
        let rendered = template::render_bytes(&raw, vars);
        let body: Value = serde_json::from_slice(&rendered)
            .with_context(|| format!("parsing query body {}", path.display()))?;

        let marker_pointer = format!("{}/{}", query.marker_pointer, expect.marker);
        let deadline = Instant::now() + Duration::from_millis(query.poll.timeout_ms);
        let mut last: Option<Vec<String>> = None;
        loop {
            let response = self.send_json(&query.request, vars, &body)?;
            if (200..300).contains(&response.status) {
                if let Ok(value) = serde_json::from_slice::<Value>(&response.body) {
                    let rows =
                        crate::query::returned_markers(&value, &query.records, &marker_pointer);
                    // Two identical non-empty reads mean the index has settled.
                    //
                    // Non-empty matters. An empty result is also stable, and
                    // treating it as settled returned nothing after two polls
                    // against a store whose commit timeout is 60s — which reads
                    // as every row missing, an ALTER, and a false finding of
                    // silent data loss. A check that genuinely expects no rows
                    // therefore waits out the timeout, which is slow and right.
                    if !rows.is_empty() && last.as_deref() == Some(rows.as_slice()) {
                        return Ok(rows);
                    }
                    last = Some(rows);
                }
            }
            if Instant::now() >= deadline {
                return Ok(last.unwrap_or_default());
            }
            std::thread::sleep(Duration::from_millis(query.poll.interval_ms));
        }
    }

    /// Sends a request that carries its own body in the adapter, such as setup
    /// or teardown, rather than a payload from a case.
    fn send_declared(&self, req: &Request, vars: &Vars) -> Result<Response> {
        match req.body.as_ref() {
            Some(body) => {
                let rendered = template::render_json(body, vars);
                self.send_json(req, vars, &rendered)
            }
            None => self.send(req, vars, Vec::new()),
        }
    }

    fn send_json(&self, req: &Request, vars: &Vars, body: &Value) -> Result<Response> {
        let mut with_type = req.clone();
        with_type
            .headers
            .entry("Content-Type".to_string())
            .or_insert_with(|| "application/json".to_string());
        self.send(&with_type, vars, body.to_string().into_bytes())
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
                let rendered = template::render_json(body, vars);
                builder = builder
                    .header("Content-Type", "application/json")
                    .body(rendered.to_string());
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
        let response = self.send(&req, &Vars::new(), Vec::new())?;
        if !(200..300).contains(&response.status) {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
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

/// Renders what a store reported about a write it accepted, for appending to a
/// result line. Empty when the store reported nothing, which is the ordinary
/// case and should add no noise.
///
/// A store that answers 200 and names what it dropped has behaved better than
/// one that answers 200 and says nothing, even though both lost the data. The
/// verdict stays the same, because the record is gone either way and the caller
/// who trusted the status is equally wrong; the detail says which of the two
/// happened, so the matrix can show it.
fn describe_report(report: Option<&crate::otlp::ExportReport>) -> String {
    let Some(report) = report else {
        return String::new();
    };
    let mut parts = Vec::new();
    if report.rejected > 0 {
        let message = if report.message.is_empty() {
            String::new()
        } else {
            format!(": {}", first_line(&report.message))
        };
        parts.push(format!("store reported {} rejected record(s){message}", report.rejected));
    }
    if let Some(mismatch) = &report.encoding_mismatch {
        parts.push(format!("{mismatch}, so no conformant client can read that report"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("; {}", parts.join("; "))
}

/// A check names its fields in the protocol's own vocabulary, so which reader
/// applies is a property of the protocol and not of the backend. New protocols
/// add an arm here rather than a special case anywhere else.
fn logical_field_for(
    protocol: &str,
    sent: &serde_json::Value,
    field: &str,
) -> Option<serde_json::Value> {
    match protocol {
        "es-bulk" => crate::es::logical_field(sent, field),
        _ => crate::otlp::logical_field(sent, field),
    }
}

/// Decides a case at ingest, or returns `None` to carry on to read-back.
///
/// `accepted-or-rejected` exists for checks where either answer at the door is
/// conformant and the question is what happens afterwards. A store that refuses
/// a record it will not keep has told the caller, which is a PASS; the failure
/// such a check looks for is 200 followed by absence, and only read-back can
/// see that.
fn ingest_verdict(expected: &str, accepted: bool) -> Result<Option<Verdict>> {
    Ok(match (expected, accepted) {
        ("accepted", true) => None,
        ("accepted", false) => Some(Verdict::Reject),
        ("rejected", true) => Some(Verdict::Alter),
        ("rejected", false) => Some(Verdict::Pass),
        ("accepted-or-rejected", true) => None,
        ("accepted-or-rejected", false) => Some(Verdict::Pass),
        (other, _) => anyhow::bail!(
            "declares expect.ingest: {other:?}; expected `accepted`, `rejected` \
             or `accepted-or-rejected`"
        ),
    })
}

/// Time-derived template variables, as a free function so they can be tested
/// without a Runner, a Backend or a network.
///
/// Payload timestamps are generated rather than fixed. A corpus carrying a
/// fixed instant ages out of a store's ingest window, and then a check measures
/// the fixture rather than the backend — which is what the 0.1 OpenObserve
/// adapter had to widen `ZO_INGEST_ALLOWED_UPTO` to work around.
fn time_vars(now: chrono::DateTime<chrono::Utc>) -> Vec<(&'static str, String)> {
    let nanos = now.timestamp_nanos_opt().unwrap_or_default() as i128;
    let day: i128 = 86_400_000_000_000;
    // Truncated to the second, then a fixed sub-second remainder. A live
    // nanosecond clock is non-deterministic in exactly the way a precision
    // check cannot tolerate: land on a whole millisecond and the check reports
    // no precision loss, which reads as a store that kept nanoseconds. These
    // low digits are known, so the check can say which of them a store dropped.
    let fractional = now.timestamp() as i128 * 1_000_000_000 + 123_456_789;
    // The search window is wide because the record is found by its run key
    // rather than by when it claims to have happened, and one case deliberately
    // sends an instant a month old.
    let start_us = (now - chrono::Duration::days(730)).timestamp_micros();
    let end_us = (now + chrono::Duration::days(1)).timestamp_micros();
    vec![
        ("window_start", (now - chrono::Duration::minutes(5)).to_rfc3339()),
        ("window_end", (now + chrono::Duration::minutes(5)).to_rfc3339()),
        // Some stores take the search window as microseconds since the epoch
        // rather than RFC 3339.
        ("window_start_us", start_us.to_string()),
        ("window_end_us", end_us.to_string()),
        ("now_ns", nanos.to_string()),
        ("now_ns_fractional", fractional.to_string()),
        ("now_us", now.timestamp_micros().to_string()),
        ("now_ms", now.timestamp_millis().to_string()),
        ("now_s", now.timestamp().to_string()),
        ("now_minus_1d_ns", (nanos - day).to_string()),
        ("now_minus_30d_ns", (nanos - 30 * day).to_string()),
    ]
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
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return (value, false);
    }
    // NDJSON is several JSON documents rather than one, so a `_bulk` body fails
    // a strict parse without being malformed. Try it before concluding the
    // payload is not valid UTF-8, or every bulk case reads as lossy and every
    // field in it reads as absent.
    if let Ok(value) = crate::es::parse_bulk(bytes) {
        return (value, false);
    }
    let lossy = String::from_utf8_lossy(bytes);
    (serde_json::from_str(&lossy).unwrap_or(serde_json::Value::Null), true)
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

    /// NDJSON is not JSON. A `_bulk` body must not be mistaken for a payload
    /// carrying invalid UTF-8, which would make every field read as absent.
    #[test]
    fn parse_sent_reads_ndjson_without_calling_it_lossy() {
        let (value, lossy) = parse_sent(b"{\"index\":{}}\n{\"message\":\"hi\"}\n");
        assert!(!lossy);
        assert_eq!(crate::es::logical_field(&value, "message"), Some(json!("hi")));
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

    /// A payload carrying a fixed historical instant ages out of a store's
    /// ingest window, and then the check is testing the fixture rather than the
    /// backend. These are generated per run.
    #[test]
    fn now_ns_is_close_to_now() {
        let now = chrono::Utc::now();
        let vars: std::collections::HashMap<_, _> = time_vars(now).into_iter().collect();
        let sent: i128 = vars["now_ns"].parse().expect("an integer");
        let actual = now.timestamp_nanos_opt().unwrap() as i128;
        assert!((sent - actual).abs() < 1_000_000_000, "sent {sent}, now {actual}");
    }

    /// A live nanosecond clock is non-deterministic in exactly the way the
    /// precision check cannot tolerate: land on a whole millisecond and it
    /// reports no precision loss, which reads as a store that kept nanoseconds.
    /// Truncate to the second, then add a fixed remainder.
    #[test]
    fn now_ns_fractional_carries_known_low_digits() {
        let now = chrono::Utc::now();
        let vars: std::collections::HashMap<_, _> = time_vars(now).into_iter().collect();
        let value = &vars["now_ns_fractional"];
        assert!(value.ends_with("123456789"), "got {value}");
        let seconds: i64 = value[..value.len() - 9].parse().expect("an integer");
        assert_eq!(seconds, now.timestamp());
    }

    #[test]
    fn microseconds_and_milliseconds_are_offered() {
        let now = chrono::Utc::now();
        let vars: std::collections::HashMap<_, _> = time_vars(now).into_iter().collect();
        assert_eq!(vars["now_ms"], now.timestamp_millis().to_string());
        assert_eq!(vars["now_us"], now.timestamp_micros().to_string());
        assert_eq!(vars["now_s"], now.timestamp().to_string());
    }

    /// A check that names an ingest expectation the runner does not know must
    /// fail loudly. Silently treating it as "not rejected" would let a typo
    /// publish a verdict.
    #[test]
    fn an_unknown_ingest_expectation_is_an_error() {
        let err = ingest_verdict("accpeted", true).unwrap_err();
        assert!(format!("{err}").contains("accpeted"), "{err}");
    }

    #[test]
    fn an_accepted_write_the_check_expected_continues_to_read_back() {
        assert_eq!(ingest_verdict("accepted", true).unwrap(), None);
    }

    #[test]
    fn a_refused_write_the_check_expected_to_land_is_a_reject() {
        assert_eq!(ingest_verdict("accepted", false).unwrap(), Some(Verdict::Reject));
    }

    #[test]
    fn a_refused_write_the_check_expected_to_be_refused_passes() {
        assert_eq!(ingest_verdict("rejected", false).unwrap(), Some(Verdict::Pass));
    }

    /// Accepting a payload a check expects to be refused is not a pass. It is
    /// the store taking something it said it would not.
    #[test]
    fn accepting_a_payload_the_check_expects_refused_is_an_alter() {
        assert_eq!(ingest_verdict("rejected", true).unwrap(), Some(Verdict::Alter));
    }

    /// Some checks are about what happens *after* a write, and either answer at
    /// ingest is conformant. Refusing tells the caller, so it is a PASS; the
    /// divergence such a check looks for is 200 followed by absence, which only
    /// read-back can see.
    #[test]
    fn accepted_or_rejected_passes_on_a_refusal() {
        assert_eq!(ingest_verdict("accepted-or-rejected", false).unwrap(), Some(Verdict::Pass));
    }

    #[test]
    fn accepted_or_rejected_continues_to_read_back_on_acceptance() {
        assert_eq!(ingest_verdict("accepted-or-rejected", true).unwrap(), None);
    }

    /// Deliberately outside a default ingest window, for the case that measures
    /// what a store does with data it will not keep.
    #[test]
    fn the_backdated_variables_are_the_ages_they_claim() {
        let now = chrono::Utc::now();
        let vars: std::collections::HashMap<_, _> = time_vars(now).into_iter().collect();
        let day: i128 = 86_400_000_000_000;
        let reference = now.timestamp_nanos_opt().unwrap() as i128;
        let one: i128 = vars["now_minus_1d_ns"].parse().expect("an integer");
        let thirty: i128 = vars["now_minus_30d_ns"].parse().expect("an integer");
        assert_eq!((reference - one) / day, 1);
        assert_eq!((reference - thirty) / day, 30);
    }
}
