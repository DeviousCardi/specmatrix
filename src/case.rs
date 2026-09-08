//! A check: the rule it tests, the payload to send, and what must survive the
//! round trip. Loaded from `cases/<protocol>/<id>.yaml`.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Case {
    pub id: String,
    pub protocol: String,
    pub title: String,
    /// A control establishes that the adapter and backend can carry an
    /// ordinary record. When it does not pass, no other row in the suite says
    /// anything about the backend.
    #[serde(default)]
    pub control: bool,
    /// The clause, divergence or filed bug this check exists for. Read by
    /// people rather than by the runner: a check that cannot cite why it
    /// exists does not go in, and the citation is what makes a result
    /// defensible when a maintainer disputes it.
    #[serde(default)]
    #[allow(dead_code)]
    pub rule: serde_json::Value,
    pub send: Send,
    pub expect: Expect,
    /// Verdicts recorded verbatim, with versions. Also read by people; this is
    /// where a finding lives between being observed and being filed upstream.
    #[serde(default)]
    #[allow(dead_code)]
    pub notes: String,
    /// Set by the loader: directory the case file lives in.
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct Send {
    /// The encoding the payload file is written in.
    pub format: String,
    /// Wire encodings this payload may be sent as, most preferred first. The
    /// runner converts from `format` to whichever of these the backend accepts.
    /// Empty means the payload may only be sent in the format it is written in,
    /// which is correct for a payload whose exact bytes are the point.
    #[serde(default)]
    pub encodings: Vec<String>,
    /// Payload path, relative to the repository root. Sent byte-for-byte after
    /// `{{ run_key }}` substitution.
    pub body: PathBuf,
    /// Declared by the remote-write case written in 0.1 and not read: since
    /// 0.2 the encoder owns compression, because a case that declares one the
    /// encoder also applies would apply it twice. Kept until 0.3 rewrites that
    /// case, so the file on disk stays parseable.
    #[allow(dead_code)]
    pub compression: Option<String>,
}

impl Send {
    pub fn encodings(&self) -> Vec<String> {
        if self.encodings.is_empty() {
            vec![self.format.clone()]
        } else {
            self.encodings.clone()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Expect {
    /// `accepted`, `rejected`, or `accepted-or-rejected`
    pub ingest: String,
    pub readback: Option<Readbacks>,
    /// A query-semantics check: run this query and compare which records come
    /// back. Mutually exclusive with `readback` in practice — a check asserts
    /// either what one record became or which records a query returns.
    pub query: Option<QueryExpect>,
}

#[derive(Debug, Deserialize)]
pub struct QueryExpect {
    /// Path to the query, in the protocol's own language, kept beside the case
    /// so a maintainer can send it by hand. It goes to every backend
    /// unchanged: a backend claiming to speak this protocol has to answer the
    /// protocol's queries rather than a translation of them.
    pub body: PathBuf,
    /// The marker of every record the query must return.
    pub returns: Vec<String>,
    /// Field in each stored document holding the marker.
    #[serde(default = "default_marker")]
    pub marker: String,
    /// `any` (default) compares as a set; `as-listed` asserts the order too.
    #[serde(default = "default_order")]
    pub order: String,
}

fn default_marker() -> String {
    "doc".to_string()
}

fn default_order() -> String {
    "any".to_string()
}

/// One read-back assertion, or several.
///
/// Several are needed the first time one request carries more than one thing
/// worth asserting on. `histogram-nan-count` sends a stale histogram and an
/// unrelated gauge together, and the finding is precisely that the second must
/// survive the first: one assertion says the gauge is present, another says the
/// histogram is absent, and a check that could only make one of them would be
/// testing half of what it exists to test.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Readbacks {
    One(ReadbackExpect),
    Many(Vec<ReadbackExpect>),
}

impl Readbacks {
    pub fn all(&self) -> Vec<&ReadbackExpect> {
        match self {
            Readbacks::One(one) => vec![one],
            Readbacks::Many(many) => many.iter().collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ReadbackExpect {
    /// `exact`: the record must be present and the listed fields unchanged.
    /// `present`: the record must be present; fields are reported, not judged.
    /// `absent`: the record must not be there.
    #[serde(rename = "match")]
    pub match_: String,
    #[serde(default)]
    pub on: Vec<FieldSpec>,
    /// Which series a check asserts on, by name. Protocols that send several
    /// series in one request need it; a check usually asserts on one of them.
    /// Rendered into the adapter's read-back as `{{ series }}`, so which query
    /// finds a named series stays the adapter's business.
    pub series: Option<String>,
}

/// A field a check asserts on, optionally with how to read it.
///
/// `on: [body]` compares the JSON values as they stand. `on: [{field:
/// timeUnixNano, as: timestamp}]` compares them as instants and reports the
/// precision each store kept, which is what makes two backends that returned
/// the same moment in different shapes comparable in one table.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum FieldSpec {
    Name(String),
    Typed {
        field: String,
        #[serde(rename = "as")]
        as_kind: String,
    },
}

impl FieldSpec {
    pub fn field(&self) -> &str {
        match self {
            FieldSpec::Name(name) => name,
            FieldSpec::Typed { field, .. } => field,
        }
    }

    /// The declared `as` value, if the check gave one.
    pub fn kind_name(&self) -> Option<&str> {
        match self {
            FieldSpec::Name(_) => None,
            FieldSpec::Typed { as_kind, .. } => Some(as_kind),
        }
    }
}

impl Case {
    pub fn payload_path(&self) -> PathBuf {
        // Paths in cases are written relative to the repo root. Fall back to
        // the case's own directory so `cases/` can be pointed elsewhere.
        if self.send.body.exists() {
            self.send.body.clone()
        } else {
            self.dir.join(self.send.body.file_name().unwrap_or_default())
        }
    }
}

pub fn load_suite(cases_dir: &Path, suite: &str) -> Result<Vec<Case>> {
    let dir = cases_dir.join(suite);
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "yaml" || e == "yml").unwrap_or(false))
        .collect();
    entries.sort();
    for path in entries {
        let text = std::fs::read_to_string(&path)?;
        let mut c: Case = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing case {}", path.display()))?;
        c.dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        out.push(c);
    }
    Ok(out)
}
