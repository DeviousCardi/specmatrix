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
    pub format: String,
    /// Payload path, relative to the repository root. Sent byte-for-byte after
    /// `{{ run_key }}` substitution.
    pub body: PathBuf,
    pub compression: Option<String>,
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
    pub on: Vec<String>,
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
