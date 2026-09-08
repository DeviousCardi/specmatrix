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
        for case in cases {
            let result = match self.run_case(suite, case) {
                Ok(r) => r,
                // A harness failure is not a backend verdict. Say so plainly
                // rather than reporting it as a divergence.
                Err(e) => CheckResult {
                    id: case.id.clone(),
                    title: case.title.clone(),
                    verdict: Verdict::Alter,
                    detail: format!("harness error: {e:#}"),
                },
            };
            results.push(result);
        }
        Outcome {
            backend: self.backend.name.clone(),
            backend_version,
            suite: suite.to_string(),
            url: self.base_url.clone(),
            results,
        }
    }

    fn run_case(&self, suite: &str, case: &Case) -> Result<CheckResult> {
        let vars = self.vars_for(case);
        let protocol = self
            .backend
            .protocols
            .get(suite)
            .with_context(|| format!("adapter {} has no protocol {suite}", self.backend.name))?;

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
                let sent: serde_json::Value = serde_json::from_slice(&template::render_bytes(
                    &std::fs::read(&payload_path)?,
                    &vars,
                ))
                .unwrap_or(serde_json::Value::Null);

                let mut differences = Vec::new();
                for field in &expect.on {
                    let want = crate::otlp::logical_field(&sent, field);
                    let got = self.field_of(readback, &record, field);
                    if want != got {
                        differences.push(format!(
                            "{field}: sent {}, read back {}",
                            render_opt(&want),
                            render_opt(&got)
                        ));
                    }
                }
                if differences.is_empty() {
                    Ok(self.result(case, Verdict::Pass, format!("{status}, round trip intact")))
                } else {
                    Ok(self.result(case, Verdict::Alter, differences.join("; ")))
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
                    if let Some(record) = first_record(&value, &readback.records) {
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
        Ok(value.get(&vf.field).and_then(|v| v.as_str()).map(str::to_string))
    }
}

/// Pulls the first record out of a read-back response.
fn first_record(value: &serde_json::Value, records_pointer: &str) -> Option<serde_json::Value> {
    let node = if records_pointer.is_empty() {
        value
    } else {
        value.pointer(records_pointer)?
    };
    match node {
        serde_json::Value::Array(items) => items.first().cloned(),
        serde_json::Value::Object(_) => Some(node.clone()),
        _ => None,
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
