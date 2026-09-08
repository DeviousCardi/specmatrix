//! Sends each case, reads it back, and decides a verdict.
//!
//! The interesting part is not sending. It is deciding, once a backend has
//! accepted a write, whether what comes back is the same thing. A backend that
//! refuses a payload tells you so; one that alters it does not.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

use crate::backend::{Auth, Backend, Readback, Request};
use crate::case::Case;
use crate::template::{self, Vars};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub title: String,
    pub verdict: Verdict,
    /// One line saying what happened. Shown beside the verdict.
    pub detail: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub backend: String,
    pub backend_version: Option<String>,
    /// The image the adapter pins. Recorded beside the reported version
    /// because the two can disagree, and a reader needs to know which build a
    /// column actually describes.
    pub backend_image: Option<String>,
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
        let client =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?;
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
            backend_image: self.backend.image(),
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
        let protocol =
            self.backend.protocols.get(suite).with_context(|| {
                format!("adapter {} has no protocol {suite}", self.backend.name)
            })?;

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
        let teardown = protocol.teardown.as_ref().or(self.backend.teardown.as_ref());
        let setup = protocol.setup.as_ref().or(self.backend.setup.as_ref());
        let setup_verify = protocol.setup_verify.as_ref().or(self.backend.setup_verify.as_ref());

        if let Some(teardown) = teardown {
            let _ = self.send_declared(teardown, &vars);
        }
        // Then recreate whatever the store needs before it will accept a write.
        // A store that will not create an index on write has to be given one,
        // and the shape of that index belongs to the adapter: putting it in the
        // corpus would make a shared case carry one backend's schema.
        if let Some(setup) = setup {
            let response = self.send_declared(setup, &vars)?;
            if !(200..300).contains(&response.status) && response.status != 400 {
                // 400 is tolerated because several stores answer it for
                // "already exists", which is not a failure to set up.
                return Ok(self.result(
                    case,
                    Verdict::NotApplicable,
                    format!("setup failed: {} {}", response.status, first_line(&response.text())),
                ));
            }
        }
        // Independent of setup. A store that creates what it needs by itself
        // still has to have finished doing so, and a case measured before it
        // has reports the store's start-up as the store's behaviour.
        if let Some(verify) = setup_verify {
            if !self.wait_until_ready(verify, &vars)? {
                return Ok(self.result(
                    case,
                    Verdict::NotApplicable,
                    "the backend's preconditions did not hold within the adapter's timeout"
                        .to_string(),
                ));
            }
        }

        // The encoding's own required headers, filled in where the adapter
        // has not declared them. A remote-write body without
        // `Content-Encoding: snappy` is unidentifiable, and the receiver
        // answers a decompression error that names neither the header nor us.
        let mut ingest = protocol.ingest.clone();
        for (name, value) in crate::encode::headers_for(&encoding) {
            ingest.headers.entry(name.to_string()).or_insert_with(|| value.to_string());
        }

        let response = self.send(&ingest, &vars, body)?;
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

        let Some(expects) = case.expect.readback.as_ref() else {
            return Ok(self.result(
                case,
                Verdict::Pass,
                format!("{status}, no read-back declared{reported}"),
            ));
        };
        let Some(readback) = protocol.readback.as_ref() else {
            return Ok(self.result(
                case,
                Verdict::Pass,
                format!("{status}, adapter declares no read-back{reported}"),
            ));
        };

        // Every assertion the case makes, each with its own read. A case with
        // one is the ordinary shape and reads exactly as it did before; a case
        // with several fails if any of them does, and the line names which.
        let mut verdict = Verdict::Pass;
        let mut details = Vec::new();
        for expect in expects.all() {
            let mut vars = vars.clone();
            if let Some(series) = expect.series.as_deref() {
                vars.insert("series", series.to_string());
                // Built here rather than in the adapter because only the
                // runner knows both halves: the case names the series, the
                // adapter names the run-key label, and which of PromQL's two
                // selector forms is legal depends on the name.
                vars.insert(
                    "series_selector",
                    crate::remote_write::series_selector(
                        series,
                        self.backend.run_key_field.as_deref(),
                        vars.get("run_key").map(String::as_str).unwrap_or_default(),
                    ),
                );
            }
            let (one, detail) = self
                .evaluate_readback(case, expect, readback, &vars, status, &reported, &rendered)?;
            if one == Verdict::Alter {
                verdict = Verdict::Alter;
            }
            details.push(detail);
        }
        Ok(self.result(case, verdict, details.join(" | ")))
    }

    /// Reads one assertion back and decides what it says.
    ///
    /// Split out of `run_case` when a case gained the ability to make more than
    /// one: the decision is per-assertion, and the case's verdict is the worst
    /// of them.
    #[allow(clippy::too_many_arguments)]
    fn evaluate_readback(
        &self,
        case: &Case,
        expect: &crate::case::ReadbackExpect,
        readback: &Readback,
        vars: &Vars,
        status: u16,
        reported: &str,
        rendered: &[u8],
    ) -> Result<(Verdict, String)> {
        // Named when the case named a series, so a line covering several
        // assertions says which one each half is about.
        let label = expect.series.as_deref().map(|s| format!("{s}: ")).unwrap_or_default();
        let found = self.read_back(readback, vars)?;

        // `absent` asserts that a record is *not* there. It is the only way to
        // check a write whose purpose is to end something — a remote-write
        // stale marker, a deletion — and `read_back` already waits out the full
        // timeout before concluding nothing arrived, which is what stops a
        // store slower than the poll from reading as compliant here.
        if expect.match_ == "absent" {
            return Ok(match found {
                None => (Verdict::Pass, format!("{label}absent, as the check requires")),
                Some(_) => (
                    Verdict::Alter,
                    format!("{label}still queryable when the check requires it to be gone"),
                ),
            });
        }

        match found {
            None => Ok((
                Verdict::Alter,
                format!("{label}accepted ({status}) but never became queryable{reported}"),
            )),
            Some(record) => {
                let (sent, sent_was_lossy) = parse_sent(rendered);

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
                        logical_field_for(&case.protocol, &sent, field, expect.series.as_deref()),
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
                    return Ok((Verdict::Alter, format!("{label}{}", replaced.join("; "))));
                }

                match expect.match_.as_str() {
                    "exact" if observed.is_empty() => {
                        Ok((Verdict::Pass, format!("{label}{status}, round trip intact{reported}")))
                    }
                    "exact" => Ok((Verdict::Alter, format!("{label}{}", observed.join("; ")))),
                    // `present` is for values the project has not adjudicated.
                    // Show what the backend stored so the divergence is visible,
                    // but do not call it a failure on the strength of a guess.
                    "present" => {
                        Ok((Verdict::Pass, format!("{label}recorded — {}", observed.join("; "))))
                    }
                    other => anyhow::bail!(
                        "case {} declares readback.match: {other:?}; expected `exact`, \
                         `present` or `absent`",
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
        let run_key = format!("sm-{:x}", rand::random::<u64>());
        // One stream per case keeps a failure in one check from contaminating
        // the next, and makes teardown a single call.
        let stream = format!("specmatrix_{}", case.id.replace(['/', '-', '.'], "_").to_lowercase());
        let mut vars = Vars::new();
        vars.insert("run_key", run_key);
        vars.insert("suite_stream", stream);
        if let Some(field) = self.backend.run_key_field.clone() {
            vars.insert("run_key_field", field);
        }
        for (key, value) in time_vars(chrono::Utc::now(), self.backend.lookback_days.unwrap_or(730))
        {
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
        // Percent-encoded by reqwest. A LogQL selector carries braces, quotes
        // and pipes, and pasting one into the path produces a 400 before the
        // store ever sees it.
        if !req.params.is_empty() {
            let mut params: Vec<(String, String)> =
                req.params.iter().map(|(k, v)| (k.clone(), template::render(v, vars))).collect();
            params.sort();
            builder = builder.query(&params);
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

    /// Polls a precondition until the store answers 2xx.
    ///
    /// Returns false if it never does, which is a reason not to measure rather
    /// than a verdict: the case was never put to the backend.
    fn wait_until_ready(&self, verify: &crate::backend::Verify, vars: &Vars) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_millis(verify.poll.timeout_ms);
        loop {
            if let Ok(response) = self.send_declared(&verify.request, vars) {
                if (200..300).contains(&response.status) {
                    return Ok(true);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(verify.poll.interval_ms));
        }
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
            // Built by `send_declared`, not by hand. This loop used to
            // reassemble the request itself and so quietly ignored anything
            // `send` learned to do — query parameters among them, which made a
            // correct Loki adapter read as a store that had lost the record.
            if let Ok(response) = self.send_declared(&readback.request, vars) {
                if let Ok(value) = serde_json::from_slice::<Value>(&response.body) {
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
        let req = Request {
            request: vf.request.clone(),
            headers: Default::default(),
            params: Default::default(),
            body: None,
        };
        let response = self.send(&req, &Vars::new(), Vec::new())?;
        if !(200..300).contains(&response.status) {
            return Ok(None);
        }
        let text = response.text();
        if let Some(pattern) = vf.pattern.as_deref() {
            let regex = regex::Regex::new(pattern)
                .with_context(|| format!("version_from.pattern {pattern:?}"))?;
            return Ok(version_from_text(&regex, &text));
        }
        let Some(field) = vf.field.as_deref() else {
            anyhow::bail!("version_from needs either a `field` or a `pattern`");
        };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        // A version may sit at the top level or be nested. Accept both, as the
        // adapter's `field` is documented to allow either.
        let found = if field.starts_with('/') { value.pointer(field) } else { value.get(field) };
        Ok(found.and_then(|v| v.as_str()).map(str::to_string))
    }
}

/// Pulls a version out of a text response using the adapter's pattern.
fn version_from_text(regex: &regex::Regex, text: &str) -> Option<String> {
    regex.captures(text)?.get(1).map(|m| m.as_str().to_string())
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
    series: Option<&str>,
) -> Option<serde_json::Value> {
    match protocol {
        "es-bulk" => crate::es::logical_field(sent, field),
        "remote-write" => crate::remote_write::logical_field(sent, field, series),
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
fn time_vars(
    now: chrono::DateTime<chrono::Utc>,
    lookback_days: i64,
) -> Vec<(&'static str, String)> {
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
    let start = now - chrono::Duration::days(lookback_days);
    let start_us = start.timestamp_micros();
    let end_us = (now + chrono::Duration::days(1)).timestamp_micros();
    vec![
        ("window_start", (now - chrono::Duration::minutes(5)).to_rfc3339()),
        ("window_end", (now + chrono::Duration::minutes(5)).to_rfc3339()),
        // Some stores take the search window as microseconds since the epoch
        // rather than RFC 3339.
        ("window_start_us", start_us.to_string()),
        ("window_end_us", end_us.to_string()),
        ("window_start_ns", start.timestamp_nanos_opt().unwrap_or_default().to_string()),
        (
            "window_end_ns",
            ((now + chrono::Duration::days(1)).timestamp_nanos_opt().unwrap_or_default())
                .to_string(),
        ),
        ("now_ns", nanos.to_string()),
        ("now_ns_fractional", fractional.to_string()),
        ("now_us", now.timestamp_micros().to_string()),
        ("now_ms", now.timestamp_millis().to_string()),
        ("now_s", now.timestamp().to_string()),
        // Milliseconds, because remote-write timestamps are milliseconds. A
        // series needs an earlier sample to be ended by a later one, and both
        // have to be inside the store's ingest window.
        ("now_minus_10s_ms", (now - chrono::Duration::seconds(10)).timestamp_millis().to_string()),
        ("now_minus_1h_ms", (now - chrono::Duration::hours(1)).timestamp_millis().to_string()),
        ("now_plus_1h_ms", (now + chrono::Duration::hours(1)).timestamp_millis().to_string()),
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
    let node = if records_pointer.is_empty() { value } else { value.pointer(records_pointer)? };
    let items: Vec<&serde_json::Value> = match node {
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(_) => vec![node],
        _ => return None,
    };
    items.into_iter().find(|record| record.to_string().contains(run_key)).cloned()
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
    if line.len() > 120 {
        format!("{}…", &line[..120])
    } else {
        line.to_string()
    }
}

/// End-to-end tests of the verdict pipeline against a stub backend.
///
/// These exist because every decision the runner makes was previously
/// observable only by pointing it at a real container: whether an absent record
/// becomes an ALTER, whether a failing control stops the suite, whether
/// teardown runs before ingest. A store that silently drops a record does so on
/// its own schedule, so the interesting cases could not be reproduced on
/// demand. Here they can be described exactly.
#[cfg(test)]
mod pipeline {
    use super::*;
    use crate::stub::{self, Reply};

    /// An adapter pointed at the stub. `readback` is the response body the
    /// query returns, in order; the last one repeats.
    fn runner_for(url: &str, _readback: Vec<Reply>, _ingest: Reply) -> Runner {
        let adapter: Backend = serde_yaml::from_str(
            r#"
name: stub
protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      request: POST /ingest
    readback:
      request: POST /search
      records: /hits
      fields:
        body: /message
        severityText: /severity
      poll:
        interval_ms: 10
        timeout_ms: 120
"#,
        )
        .expect("adapter parses");
        Runner::new(adapter, url.to_string(), false).expect("runner builds")
    }

    fn case_yaml(extra: &str) -> Case {
        let mut case: Case = serde_yaml::from_str(&format!(
            r#"
id: otlp-logs/minimal-record
protocol: otlp-logs
title: stub case
send:
  format: otlp-json
  body: cases/otlp-logs/minimal-record.json
expect:
  ingest: accepted
{extra}
"#
        ))
        .expect("case parses");
        case.dir = std::path::PathBuf::from("cases/otlp-logs");
        case
    }

    /// A write that lands and reads back unchanged is the only thing that
    /// should produce a PASS. The stub echoes the run key the runner generated,
    /// because it is random per case and no canned record could match it.
    #[test]
    fn a_record_that_reads_back_unchanged_passes() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/search",
                vec![Reply::json(
                    200,
                    r#"{"hits":[{"message":"specmatrix minimal record","severity":"INFO","specmatrix.run":"{{RUNKEY}}"}]}"#,
                )],
            ),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: exact\n    on: [body, severityText]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Pass, "detail: {}", result.detail);
        assert!(result.detail.contains("round trip intact"), "{}", result.detail);
    }

    /// A value that comes back changed is an ALTER naming both sides, so a
    /// reader can see what happened without rerunning anything.
    #[test]
    fn a_changed_value_is_an_alter_naming_what_was_sent_and_read() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/search",
                vec![Reply::json(
                    200,
                    r#"{"hits":[{"message":"TRUNCATED","severity":"INFO","specmatrix.run":"{{RUNKEY}}"}]}"#,
                )],
            ),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter);
        assert!(result.detail.contains("specmatrix minimal record"), "{}", result.detail);
        assert!(result.detail.contains("TRUNCATED"), "{}", result.detail);
    }

    /// `present` reports the stored value and does not judge it. This is how
    /// the corpus holds a divergence it has not adjudicated.
    #[test]
    fn present_mode_reports_a_difference_without_calling_it_a_failure() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/search",
                vec![Reply::json(
                    200,
                    r#"{"hits":[{"message":"something else","severity":"INFO","specmatrix.run":"{{RUNKEY}}"}]}"#,
                )],
            ),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: present\n    on: [body]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Pass);
        assert!(result.detail.starts_with("recorded"), "{}", result.detail);
        assert!(result.detail.contains("something else"), "{}", result.detail);
    }

    /// A record left behind by an earlier run must not be mistaken for this
    /// one's, or a store that dropped the write looks like it kept it.
    #[test]
    fn a_leftover_record_from_another_run_does_not_count() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/search",
                vec![Reply::json(
                    200,
                    r#"{"hits":[{"message":"from an earlier run","severity":"INFO","specmatrix.run":"sm-deadbeef"}]}"#,
                )],
            ),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter);
        assert!(result.detail.contains("never became queryable"), "{}", result.detail);
    }

    /// Most stores acknowledge a write before it is queryable, so read-back
    /// polls. A store that is merely slow must not be reported as one that
    /// lost the record.
    #[test]
    fn a_record_that_appears_after_a_poll_still_passes() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/search",
                vec![
                    Reply::json(200, r#"{"hits":[]}"#),
                    Reply::json(200, r#"{"hits":[]}"#),
                    Reply::json(
                        200,
                        r#"{"hits":[{"message":"specmatrix minimal record","severity":"INFO","specmatrix.run":"{{RUNKEY}}"}]}"#,
                    ),
                ],
            ),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Pass, "detail: {}", result.detail);
    }

    /// Accepted, then never queryable. The failure the project exists for, and
    /// the one a caller cannot see.
    #[test]
    fn a_record_that_never_becomes_queryable_is_an_alter() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            ("/search", vec![Reply::json(200, r#"{"hits":[]}"#)]),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter);
        assert!(result.detail.contains("never became queryable"), "{}", result.detail);
    }

    /// `absent` passes when nothing arrives. It waits out the whole poll
    /// timeout first: a store slower than one read would otherwise look
    /// compliant for the wrong reason, which is the failure mode that produced
    /// a false finding in 0.2's query path.
    #[test]
    fn absent_passes_when_the_record_never_appears() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            ("/search", vec![Reply::json(200, r#"{"hits":[]}"#)]),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: absent");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Pass, "detail: {}", result.detail);
        assert!(result.detail.contains("absent"), "{}", result.detail);
        assert!(stub.paths().iter().filter(|p| p.contains("/search")).count() > 1, "polled once");
    }

    /// A write whose purpose was to end a series, and the series is still
    /// there. Nothing errored, so only a read can see it.
    #[test]
    fn absent_is_an_alter_when_the_record_is_still_there() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/search",
                vec![Reply::json(
                    200,
                    r#"{"hits":[{"message":"x","specmatrix.run":"{{RUNKEY}}"}]}"#,
                )],
            ),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: absent");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter, "detail: {}", result.detail);
        assert!(result.detail.contains("still queryable"), "{}", result.detail);
    }

    /// One request, two assertions. The case fails if either does, and the
    /// line says which — the shape `histogram-nan-count` needs, where a stale
    /// series must not cost the unrelated one sent beside it.
    #[test]
    fn several_readbacks_are_all_asserted_and_the_worst_verdict_wins() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/search",
                vec![Reply::json(
                    200,
                    r#"{"hits":[{"message":"specmatrix minimal record","specmatrix.run":"{{RUNKEY}}"}]}"#,
                )],
            ),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        // The first assertion holds, the second does not: the record the stub
        // returns is present, and the check requires it gone.
        let case = case_yaml(
            "  readback:\n    - match: exact\n      series: kept\n      on: [body]\n    - match: absent\n      series: ended",
        );
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter, "detail: {}", result.detail);
        assert!(result.detail.contains("kept: "), "{}", result.detail);
        assert!(result.detail.contains("ended: still queryable"), "{}", result.detail);
    }

    /// A single read-back keeps working written the way every existing case
    /// writes it, so gaining a list changed no corpus file.
    #[test]
    fn a_single_readback_still_parses_and_reads_the_same() {
        let case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        assert_eq!(case.expect.readback.as_ref().unwrap().all().len(), 1);
    }

    /// A refusal is a REJECT carrying the status and the store's own words, so
    /// a reader can act on it without rerunning anything.
    #[test]
    fn a_refused_write_is_a_reject_naming_the_status_and_message() {
        let stub = stub::start(vec![(
            "/ingest",
            vec![Reply::json(400, r#"{"error":"invalid unicode code point"}"#)],
        )]);
        let runner = runner_for(&stub.url, vec![], Reply::json(400, ""));
        let case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Reject);
        assert!(result.detail.starts_with("400"), "{}", result.detail);
        assert!(result.detail.contains("invalid unicode"), "{}", result.detail);
    }

    /// A store that accepts a payload the check expects to be refused has taken
    /// something it said it would not.
    #[test]
    fn accepting_a_payload_the_check_expects_refused_is_an_alter() {
        let stub = stub::start(vec![("/ingest", vec![Reply::json(200, "{}")])]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let mut case = case_yaml("");
        case.expect.ingest = "rejected".to_string();
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter);
    }

    /// `accepted-or-rejected` passes on a refusal, because the store told the
    /// caller. The divergence such a check looks for is 200 then absence.
    #[test]
    fn accepted_or_rejected_passes_when_the_store_refuses() {
        let stub = stub::start(vec![(
            "/ingest",
            vec![Reply::json(400, r#"{"error":"timestamp too old"}"#)],
        )]);
        let runner = runner_for(&stub.url, vec![], Reply::json(400, ""));
        let mut case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        case.expect.ingest = "accepted-or-rejected".to_string();
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Pass);
        assert!(result.detail.contains("timestamp too old"), "{}", result.detail);
    }

    /// A store that answers 200 and names what it dropped is doing something
    /// different from one that says nothing, and the row must show it.
    #[test]
    fn a_reported_rejection_is_carried_into_the_detail() {
        let stub = stub::start(vec![
            (
                "/ingest",
                vec![Reply::json(
                    200,
                    r#"{"partialSuccess":{"rejectedLogRecords":"1","errorMessage":"too old, discarded"}}"#,
                )],
            ),
            ("/search", vec![Reply::json(200, r#"{"hits":[]}"#)]),
        ]);
        let runner = runner_for(&stub.url, vec![], Reply::json(200, "{}"));
        let case = case_yaml("  readback:\n    match: exact\n    on: [body]");
        let result = runner.run_case("otlp-logs", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter);
        assert!(result.detail.contains("1 rejected record"), "{}", result.detail);
        assert!(result.detail.contains("too old, discarded"), "{}", result.detail);
    }

    /// teardown was declared in the adapter schema from 0.1 and never sent, so
    /// a rerun read whatever the last run left behind. It must run first.
    #[test]
    fn teardown_runs_before_ingest() {
        let stub = stub::start(vec![
            ("/reset", vec![Reply::json(200, "{}")]),
            ("/ingest", vec![Reply::json(200, "{}")]),
            ("/search", vec![Reply::json(200, r#"{"hits":[]}"#)]),
        ]);
        let adapter: Backend = serde_yaml::from_str(
            r#"
name: stub
teardown:
  request: DELETE /reset
protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      request: POST /ingest
"#,
        )
        .unwrap();
        let runner = Runner::new(adapter, stub.url.clone(), false).unwrap();
        let case = case_yaml("");
        runner.run_case("otlp-logs", &case).expect("no harness error");
        let paths = stub.paths();
        let reset = paths.iter().position(|p| p.contains("/reset")).expect("teardown sent");
        let ingest = paths.iter().position(|p| p.contains("/ingest")).expect("ingest sent");
        assert!(reset < ingest, "teardown must precede ingest, got {paths:?}");
    }

    /// A setup that reports success but has not taken effect is the race that
    /// made a correct Quickwit adapter report a rejected control: teardown had
    /// not finished deleting, so the create answered "already exists" and the
    /// write landed on an index about to vanish.
    #[test]
    fn a_setup_that_has_not_taken_effect_is_not_applicable() {
        let stub = stub::start(vec![
            ("/create", vec![Reply::json(400, r#"{"error":"already exists"}"#)]),
            ("/exists", vec![Reply::json(404, r#"{"error":"index not found"}"#)]),
            ("/ingest", vec![Reply::json(200, "{}")]),
        ]);
        let adapter: Backend = serde_yaml::from_str(
            r#"
name: stub
setup:
  request: POST /create
setup_verify:
  request: GET /exists
  poll:
    interval_ms: 10
    timeout_ms: 120
protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      request: POST /ingest
"#,
        )
        .unwrap();
        let runner = Runner::new(adapter, stub.url.clone(), false).unwrap();
        let result = runner.run_case("otlp-logs", &case_yaml("")).expect("no harness error");
        assert_eq!(result.verdict, Verdict::NotApplicable);
        assert!(result.detail.contains("did not hold"), "{}", result.detail);
        assert!(!stub.paths().iter().any(|p| p.contains("/ingest")), "must not ingest");
    }

    /// Once the precondition holds, the case is measured normally.
    #[test]
    fn a_setup_that_takes_effect_lets_the_case_run() {
        let stub = stub::start(vec![
            ("/create", vec![Reply::json(400, r#"{"error":"already exists"}"#)]),
            (
                "/exists",
                vec![Reply::json(404, "{}"), Reply::json(404, "{}"), Reply::json(200, "{}")],
            ),
            ("/ingest", vec![Reply::json(200, "{}")]),
        ]);
        let adapter: Backend = serde_yaml::from_str(
            r#"
name: stub
setup:
  request: POST /create
setup_verify:
  request: GET /exists
  poll:
    interval_ms: 10
    timeout_ms: 120
protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      request: POST /ingest
"#,
        )
        .unwrap();
        let runner = Runner::new(adapter, stub.url.clone(), false).unwrap();
        let result = runner.run_case("otlp-logs", &case_yaml("")).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Pass, "detail: {}", result.detail);
        assert!(stub.paths().iter().any(|p| p.contains("/ingest")), "must ingest once ready");
    }

    /// A store that will not create an index on write has to be given one, and
    /// a setup that fails is not a verdict about the backend.
    #[test]
    fn a_failed_setup_is_not_applicable_rather_than_a_verdict() {
        let stub = stub::start(vec![
            ("/create", vec![Reply::json(500, r#"{"error":"cannot create"}"#)]),
            ("/ingest", vec![Reply::json(200, "{}")]),
        ]);
        let adapter: Backend = serde_yaml::from_str(
            r#"
name: stub
setup:
  request: POST /create
protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      request: POST /ingest
"#,
        )
        .unwrap();
        let runner = Runner::new(adapter, stub.url.clone(), false).unwrap();
        let result = runner.run_case("otlp-logs", &case_yaml("")).expect("no harness error");
        assert_eq!(result.verdict, Verdict::NotApplicable);
        assert!(result.detail.contains("setup failed"), "{}", result.detail);
        assert!(!stub.paths().iter().any(|p| p.contains("/ingest")), "must not ingest");
    }

    /// When the control does not pass, no other row says anything about the
    /// backend, and printing one fact five times helps nobody.
    #[test]
    fn a_failing_control_marks_the_rest_of_the_suite_not_applicable() {
        let stub = stub::start(vec![("/ingest", vec![Reply::json(400, r#"{"error":"refused"}"#)])]);
        let runner = runner_for(&stub.url, vec![], Reply::json(400, ""));
        let mut control = case_yaml("");
        control.control = true;
        control.id = "otlp-logs/control".to_string();
        let mut other = case_yaml("");
        other.id = "otlp-logs/other".to_string();

        let outcome = runner.run_suite("otlp-logs", &[control, other]);
        let control_row = outcome.results.iter().find(|r| r.id.ends_with("control")).unwrap();
        let other_row = outcome.results.iter().find(|r| r.id.ends_with("other")).unwrap();
        assert_eq!(control_row.verdict, Verdict::Reject);
        assert_eq!(other_row.verdict, Verdict::NotApplicable);
        assert!(other_row.detail.contains("control rejected"), "{}", other_row.detail);
        // N/A is never counted with the verdicts.
        assert_eq!(outcome.count(Verdict::NotApplicable), 1);
        assert_eq!(outcome.count(Verdict::Pass), 0);
    }

    /// A harness failure is N/A, never a verdict: the backend has not been
    /// asked anything.
    #[test]
    fn a_network_failure_is_not_applicable_not_a_verdict() {
        let adapter: Backend = serde_yaml::from_str(
            r#"
name: stub
protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      request: POST /ingest
"#,
        )
        .unwrap();
        // Port 1 is closed; nothing is listening.
        let runner = Runner::new(adapter, "http://127.0.0.1:1".into(), false).unwrap();
        let outcome = runner.run_suite("otlp-logs", &[case_yaml("")]);
        assert_eq!(outcome.results[0].verdict, Verdict::NotApplicable);
        assert!(
            outcome.results[0].detail.starts_with("harness error"),
            "{}",
            outcome.results[0].detail
        );
    }

    /// A query check compares which rows come back, not one record's fields.
    #[test]
    fn a_query_check_reports_the_rows_that_differ() {
        let stub = stub::start(vec![
            ("/ingest", vec![Reply::json(200, "{}")]),
            (
                "/query",
                vec![Reply::json(
                    200,
                    r#"{"hits":{"hits":[{"_source":{"doc":"a"}},{"_source":{"doc":"unexpected-row"}}]}}"#,
                )],
            ),
        ]);
        let adapter: Backend = serde_yaml::from_str(
            r#"
name: stub
protocols:
  es-bulk:
    formats: [es-ndjson]
    ingest:
      request: POST /ingest
    query:
      request: POST /query
      records: /hits/hits
      marker_pointer: /_source
      poll:
        interval_ms: 10
        timeout_ms: 60
"#,
        )
        .unwrap();
        let runner = Runner::new(adapter, stub.url.clone(), false).unwrap();
        let mut case: Case = serde_yaml::from_str(
            r#"
id: es-bulk/stub
protocol: es-bulk
title: stub query case
send:
  format: es-ndjson
  body: cases/es-bulk/absent-field.ndjson
expect:
  ingest: accepted
  query:
    body: cases/es-bulk/match-all-run.query.json
    returns: [a]
"#,
        )
        .unwrap();
        case.dir = std::path::PathBuf::from("cases/es-bulk");
        let result = runner.run_case("es-bulk", &case).expect("no harness error");
        assert_eq!(result.verdict, Verdict::Alter);
        assert!(result.detail.contains("unexpected: unexpected-row"), "{}", result.detail);
    }
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

    /// Several stores report their version only on a Prometheus metrics
    /// endpoint, which is text rather than JSON.
    #[test]
    fn a_version_can_be_read_out_of_a_text_response() {
        let regex = regex::Regex::new(r#"vm_app_version\{.*short_version="([^"]+)""#).unwrap();
        let body = concat!(
            "# HELP vm_app_version Version\n",
            "vm_app_version{version=\"victoria-logs-20260716-tags-v1.52.0\", short_version=\"v1.52.0\"} 1\n",
        );
        assert_eq!(version_from_text(&regex, body), Some("v1.52.0".to_string()));
    }

    #[test]
    fn a_pattern_that_matches_nothing_yields_no_version() {
        let regex = regex::Regex::new(r"nothing_like_this=(\d+)").unwrap();
        assert_eq!(version_from_text(&regex, "some other output"), None);
    }

    /// A payload carrying a fixed historical instant ages out of a store's
    /// ingest window, and then the check is testing the fixture rather than the
    /// backend. These are generated per run.
    #[test]
    fn now_ns_is_close_to_now() {
        let now = chrono::Utc::now();
        let vars: std::collections::HashMap<_, _> = time_vars(now, 730).into_iter().collect();
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
        let vars: std::collections::HashMap<_, _> = time_vars(now, 730).into_iter().collect();
        let value = &vars["now_ns_fractional"];
        assert!(value.ends_with("123456789"), "got {value}");
        let seconds: i64 = value[..value.len() - 9].parse().expect("an integer");
        assert_eq!(seconds, now.timestamp());
    }

    /// Some stores cap how far a query may reach back — Loki 3.1.1 refuses a
    /// range over 30d1h — so the window is an adapter's property, not a
    /// constant the corpus imposes on every store.
    #[test]
    fn the_lookback_window_is_configurable() {
        let now = chrono::Utc::now();
        let vars: std::collections::HashMap<_, _> = time_vars(now, 29).into_iter().collect();
        let start: i128 = vars["window_start_ns"].parse().unwrap();
        let days = (now.timestamp_nanos_opt().unwrap() as i128 - start) / 86_400_000_000_000;
        assert_eq!(days, 29);
    }

    #[test]
    fn microseconds_and_milliseconds_are_offered() {
        let now = chrono::Utc::now();
        let vars: std::collections::HashMap<_, _> = time_vars(now, 730).into_iter().collect();
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
        let vars: std::collections::HashMap<_, _> = time_vars(now, 730).into_iter().collect();
        let day: i128 = 86_400_000_000_000;
        let reference = now.timestamp_nanos_opt().unwrap() as i128;
        let one: i128 = vars["now_minus_1d_ns"].parse().expect("an integer");
        let thirty: i128 = vars["now_minus_30d_ns"].parse().expect("an integer");
        assert_eq!((reference - one) / day, 1);
        assert_eq!((reference - thirty) / day, 30);
    }
}
