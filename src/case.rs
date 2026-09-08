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
    #[serde(default)]
    pub rule: serde_json::Value,
    pub send: Send,
    pub expect: Expect,
    #[serde(default)]
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
    /// `accepted` or `rejected`
    pub ingest: String,
    pub readback: Option<ReadbackExpect>,
}

#[derive(Debug, Deserialize)]
pub struct ReadbackExpect {
    /// `exact`: the record must be present and the listed fields unchanged.
    /// `present`: the record must be present; fields are reported, not judged.
    #[serde(rename = "match")]
    pub match_: String,
    #[serde(default)]
    pub on: Vec<FieldSpec>,
    /// Which series a check asserts on, by name. Protocols that send several
    /// series in one request need it; a check usually asserts on one of them.
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
