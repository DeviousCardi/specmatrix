//! Minimal `{{ name }}` substitution. Adapters and cases use a handful of
//! runtime variables (stream name, run key, time window); nothing more is
//! needed and a real template engine would invite logic into config files.

use std::collections::HashMap;

pub type Vars = HashMap<&'static str, String>;

pub fn render(input: &str, vars: &Vars) -> String {
    let mut out = input.to_string();
    for (k, v) in vars {
        for pat in [format!("{{{{ {k} }}}}"), format!("{{{{{k}}}}}")] {
            out = out.replace(&pat, v);
        }
    }
    out
}

/// Byte-level substitution, for payloads that are deliberately not valid UTF-8.
pub fn render_bytes(input: &[u8], vars: &Vars) -> Vec<u8> {
    let mut out = input.to_vec();
    for (k, v) in vars {
        for pat in [format!("{{{{ {k} }}}}"), format!("{{{{{k}}}}}")] {
            out = replace_bytes(&out, pat.as_bytes(), v.as_bytes());
        }
    }
    out
}

fn replace_bytes(hay: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return hay.to_vec();
    }
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(needle) {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}
