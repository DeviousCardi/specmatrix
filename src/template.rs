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

/// Renders a JSON document, substituting `{{ name }}` inside strings.
///
/// A string that is *exactly* one placeholder becomes a number when its value
/// parses as one. Adapters need this because some search APIs take a numeric
/// time window, and YAML cannot hold a placeholder as anything but a string.
/// Strings that merely contain a placeholder are left as strings, so a SQL
/// clause with a run key embedded in it is unaffected.
pub fn render_json(value: &serde_json::Value, vars: &Vars) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => {
            let rendered = render(text, vars);
            if is_sole_placeholder(text) {
                if let Ok(number) = rendered.parse::<i64>() {
                    return Value::from(number);
                }
                if let Ok(number) = rendered.parse::<f64>() {
                    return Value::from(number);
                }
            }
            Value::String(rendered)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| render_json(item, vars)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, val)| (key.clone(), render_json(val, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn is_sole_placeholder(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("{{")
        && trimmed.ends_with("}}")
        && !trimmed[2..trimmed.len() - 2].contains("{{")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars() -> Vars {
        let mut vars = Vars::new();
        vars.insert("window_start_us", "1755000000000000".to_string());
        vars.insert("run_key", "sm-abc".to_string());
        vars
    }

    #[test]
    fn a_sole_numeric_placeholder_becomes_a_number() {
        let rendered = render_json(&json!({"start_time": "{{ window_start_us }}"}), &vars());
        assert_eq!(rendered["start_time"], json!(1_755_000_000_000_000i64));
        assert!(rendered["start_time"].is_number());
    }

    /// A placeholder embedded in a larger string stays a string, or a SQL
    /// clause carrying a run key would be turned into a bare number.
    #[test]
    fn an_embedded_placeholder_stays_a_string() {
        let rendered = render_json(&json!({"sql": "WHERE run = '{{ run_key }}'"}), &vars());
        assert_eq!(rendered["sql"], json!("WHERE run = 'sm-abc'"));
    }

    #[test]
    fn a_non_numeric_placeholder_stays_a_string() {
        let rendered = render_json(&json!({"key": "{{ run_key }}"}), &vars());
        assert_eq!(rendered["key"], json!("sm-abc"));
    }
}
