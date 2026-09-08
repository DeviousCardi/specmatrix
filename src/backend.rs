//! A backend adapter, loaded from `backends/<name>.yaml`. It supplies the
//! ingest endpoint, auth, the read-back query, and the fields the backend
//! legitimately adds. Everything else is shared by the runner.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Backend {
    pub name: String,
    pub version_from: Option<VersionFrom>,
    /// How to start this backend. Typed rather than opaque since 0.2, because
    /// `specmatrix up` reads it: a matrix that cannot be reproduced from the
    /// repository is a claim rather than a result.
    pub container: Option<Container>,
    pub auth: Option<Auth>,
    pub protocols: HashMap<String, Protocol>,
    /// How this backend names the run-key field in a query. A case writes
    /// `{{ run_key_field }}` so the query it carries stays portable: which
    /// column or subfield holds the key is the adapter's business, and baking
    /// one backend's spelling into a shared case would make the case a test of
    /// that backend's mapping.
    pub run_key_field: Option<String>,
    /// How far back the read-back window reaches, in days. The default is wide
    /// because a record is found by its run key rather than by when it claims
    /// to have happened. Some stores cap the range a query may span — Loki
    /// 3.1.1 refuses anything over 30d1h with a 400 — so they narrow it here
    /// rather than the corpus narrowing it for everyone.
    pub lookback_days: Option<i64>,
    #[serde(default)]
    pub normalise: Normalise,
    /// A request sent before each case's ingest, after teardown. Some stores
    /// will not create an index on write and must be given one; the shape of
    /// that index is part of the adapter, not of the corpus.
    pub setup: Option<Request>,
    pub teardown: Option<Request>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Container {
    pub image: String,
    #[serde(default)]
    pub command: Vec<String>,
    pub port: u16,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub ready: Option<Ready>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Ready {
    pub request: String,
    pub expect_status: u16,
}

#[derive(Debug, Deserialize)]
pub struct VersionFrom {
    pub request: String,
    /// JSON pointer or bare field name in the response.
    pub field: Option<String>,
    /// A regular expression with one capture group, for a store that reports
    /// its version as text rather than JSON. Several expose it only on a
    /// Prometheus metrics endpoint, and a matrix without versions is a claim
    /// about the past that reads as a claim about the present.
    pub pattern: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Auth {
    pub kind: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Protocol {
    /// Wire formats this backend accepts for the protocol. Empty means the
    /// adapter has not declared any, and every case is attempted.
    #[serde(default)]
    pub formats: Vec<String>,
    pub ingest: Request,
    pub readback: Option<Readback>,
    /// Where a query-semantics check sends its query and where the rows come
    /// back. Separate from `readback` because it asks a different question and
    /// a backend may answer it at a different endpoint.
    pub query: Option<Query>,
}

#[derive(Debug, Deserialize)]
pub struct Query {
    #[serde(flatten)]
    pub request: Request,
    /// JSON pointer to the array of rows in the response.
    #[serde(default)]
    pub records: String,
    /// JSON pointer, within one row, to the object holding the marker field.
    pub marker_pointer: String,
    #[serde(default)]
    pub poll: Poll,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Request {
    /// `METHOD /path`, path relative to the base URL
    pub request: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Query parameters, percent-encoded by the runner. Use these rather than
    /// writing a query string into `request`: a value that is safe by accident,
    /// like a hex run key, hides the fact that one containing a brace, a quote
    /// or a space is refused before it reaches the store.
    #[serde(default)]
    pub params: HashMap<String, String>,
    pub body: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Readback {
    #[serde(flatten)]
    pub request: Request,
    /// JSON pointer to the array of records in the response. Empty or absent
    /// means the response body itself is the array.
    #[serde(default)]
    pub records: String,
    /// Map from the case's field name (OTLP JSON naming) to a JSON pointer in
    /// the backend's record. Renaming a field is permitted; rewriting a value
    /// is not, and there is deliberately no place here to do it.
    #[serde(default)]
    pub fields: HashMap<String, String>,
    #[serde(default)]
    pub poll: Poll,
}

#[derive(Debug, Deserialize)]
pub struct Poll {
    #[serde(default = "default_interval")]
    pub interval_ms: u64,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

impl Default for Poll {
    fn default() -> Self {
        Self { interval_ms: default_interval(), timeout_ms: default_timeout() }
    }
}
fn default_interval() -> u64 { 500 }
fn default_timeout() -> u64 { 15_000 }

#[derive(Debug, Deserialize, Default)]
pub struct Normalise {
    #[serde(default)]
    pub drop_fields: Vec<String>,
}

impl Backend {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let b: Backend = serde_yaml::from_str(&text).context("parsing adapter YAML")?;
        Ok(b)
    }

    pub fn image(&self) -> Option<String> {
        self.container.as_ref().map(|c| c.image.clone())
    }

    /// Where this backend listens when started by `specmatrix up`, used when
    /// `run` is given no --url.
    pub fn default_url(&self) -> Option<String> {
        self.container.as_ref().map(|c| format!("http://localhost:{}", c.port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request with no params keeps working unchanged, so adapters written
    /// before this existed are unaffected.
    #[test]
    fn params_default_to_empty() {
        let req: Request = serde_yaml::from_str("request: GET /health").unwrap();
        assert!(req.params.is_empty());
    }

    #[test]
    fn params_are_read_as_written() {
        let req: Request = serde_yaml::from_str(
            "request: GET /loki/api/v1/query_range\nparams:\n  query: '{a=\"b\"}|c=\"d\"'\n  limit: \"10\"",
        )
        .unwrap();
        assert_eq!(req.params.get("query").unwrap(), "{a=\"b\"}|c=\"d\"");
        assert_eq!(req.params.get("limit").unwrap(), "10");
    }
}

impl Request {
    /// Split `METHOD /path` into its parts.
    pub fn parts(&self) -> Result<(String, String)> {
        let (m, p) = self
            .request
            .split_once(' ')
            .with_context(|| format!("request must be `METHOD /path`, got {:?}", self.request))?;
        Ok((m.trim().to_uppercase(), p.trim().to_string()))
    }
}
