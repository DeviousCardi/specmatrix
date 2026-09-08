//! A stub HTTP backend, for testing the runner without a container.
//!
//! The verdict pipeline is the part of this project most worth testing and was
//! the part least covered: every decision it makes — pass, reject, alter, the
//! control blocking a suite, teardown running before ingest — could only be
//! observed by pointing the runner at a real store. That makes the tests slow,
//! network-dependent, and unable to reproduce the interesting cases on demand,
//! because a store that silently drops a record does so on its own schedule.
//!
//! This serves canned responses and records what it was asked, so a test can
//! construct exactly the backend behaviour it wants to check.

#![cfg(test)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// What the stub should answer for one route.
#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub content_type: &'static str,
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Self {
        Reply { status, body: body.to_string(), content_type: "application/json" }
    }
}

#[derive(Debug, Clone)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub body: String,
}

pub struct Stub {
    pub url: String,
    received: Arc<Mutex<Vec<Received>>>,
}

impl Stub {
    /// Every request the stub has been sent, in order.
    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    pub fn paths(&self) -> Vec<String> {
        self.received().into_iter().map(|r| r.path).collect()
    }
}

/// Starts a stub answering the given routes.
///
/// A route matches when the request path contains its key. When several replies
/// are given for one route they are served in order and the last one repeats,
/// which is how a store that only becomes queryable after a poll or two is
/// described.
pub fn start(routes: Vec<(&'static str, Vec<Reply>)>) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let received = Arc::new(Mutex::new(Vec::new()));
    let table: HashMap<&'static str, Vec<Reply>> = routes.into_iter().collect();
    let seen = received.clone();

    std::thread::spawn(move || {
        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let Some(request) = read_request(&mut stream) else { continue };
            seen.lock().unwrap().push(request.clone());

            let matched = table.iter().find(|(route, _)| request.path.contains(**route));
            let reply = match matched {
                Some((route, replies)) => {
                    let n = counts.entry(route).or_insert(0);
                    let reply = replies[(*n).min(replies.len() - 1)].clone();
                    *n += 1;
                    reply
                }
                None => Reply {
                    status: 404,
                    body: "{\"error\":\"no stub route\"}".to_string(),
                    content_type: "application/json",
                },
            };
            // A reply may echo the run key the runner generated for this case.
            // Without it the PASS path cannot be tested at all: the key is
            // random per case, so no canned record can match one.
            let body = if reply.body.contains("{{RUNKEY}}") {
                let key = seen
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .find_map(|r| find_run_key(&r.body))
                    .unwrap_or_default();
                reply.body.replace("{{RUNKEY}}", &key)
            } else {
                reply.body.clone()
            };
            let response = format!(
                "HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reply.content_type,
                body.as_bytes().len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    Stub { url, received }
}

/// Pulls the runner's run key out of a payload it sent. Keys are `sm-` and hex,
/// which no other part of the corpus's payloads looks like.
fn find_run_key(body: &str) -> Option<String> {
    let start = body.find("sm-")?;
    let rest = &body[start + 3..];
    let end = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
    (end > 0).then(|| format!("sm-{}", &rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::find_run_key;

    #[test]
    fn a_run_key_is_found_in_a_payload() {
        let body = r#"{"key":"specmatrix.run","value":{"stringValue":"sm-2ec76f6afab11dd3"}}"#;
        assert_eq!(find_run_key(body), Some("sm-2ec76f6afab11dd3".to_string()));
    }

    #[test]
    fn a_payload_without_one_yields_nothing() {
        assert_eq!(find_run_key(r#"{"body":"no key here"}"#), None);
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Received> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(Received { method, path, body: String::from_utf8_lossy(&body).into_owned() })
}
