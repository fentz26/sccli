//! Optional diagnostics. `scc --debug` writes JSON lines to `debug.log` in the cache folder.
//! Everything passes through `redact`, because the log is what people paste into bug reports.

use crate::config;
use serde_json::{Map, Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

pub fn enable() -> Option<std::path::PathBuf> {
    let dir = config::cache_dir();
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("debug.log");
    let _ = fs::remove_file(&path); // an existing file would keep its old permissions
    let mut open = OpenOptions::new();
    open.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let _ = LOG.set(Mutex::new(open.open(&path).ok()?));
    Some(path)
}

/// Records one event. Costs a single check when logging is off.
pub fn event(name: &str, fields: Value) {
    let Some(log) = LOG.get() else { return };
    let _ = writeln!(log.lock().unwrap(), "{}", line(name, fields));
}

fn line(name: &str, fields: Value) -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut m = Map::new();
    m.insert("ts_ms".into(), json!(ms));
    m.insert("event".into(), json!(name));
    if let Value::Object(f) = scrub("", fields) {
        m.extend(f);
    }
    Value::Object(m).to_string()
}

/// Redacts at every depth. A field named like a secret loses its whole value.
fn scrub(key: &str, v: Value) -> Value {
    let key = key.to_lowercase();
    let secret = [
        "token",
        "authorization",
        "client_id",
        "secret",
        "signature",
        "policy",
        "cookie",
    ]
    .iter()
    .any(|s| key.contains(s));
    match v {
        Value::Null => Value::Null,
        _ if secret => json!("<redacted>"),
        Value::String(s) => Value::String(redact(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(|v| scrub("", v)).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| {
                    let c = scrub(&k, v);
                    (k, c)
                })
                .collect(),
        ),
        other => other,
    }
}

/// Removes what could give access: query strings (client ids, signatures, stream
/// authorizations), private link tokens, and anything that follows an OAuth marker.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut secret_next = false;
    for raw in text.split_inclusive(char::is_whitespace) {
        let word = raw.trim_end();
        let gap = &raw[word.len()..];
        if secret_next {
            out.push_str("<redacted>");
            out.push_str(gap);
            secret_next = false;
            continue;
        }
        let lower = word.to_lowercase();
        if lower.trim_end_matches(':').trim_start_matches("authorization:") == "oauth" {
            secret_next = true;
            out.push_str(word);
        } else if lower.contains("oauth_token=") || lower.contains("oauth_token:") {
            out.push_str("oauth_token=<redacted>");
        } else {
            out.push_str(&clean_word(word));
        }
        out.push_str(gap);
    }
    out
}

fn clean_word(word: &str) -> String {
    let (head, query) = match word.split_once('?') {
        Some((h, _)) if h.contains("://") || h.contains('/') => (h, true),
        _ => (word, false),
    };
    let path: Vec<&str> = head
        .split('/')
        .map(|p| {
            if p.starts_with("s-") && p.len() > 3 {
                "s-<redacted>"
            } else {
                p
            }
        })
        .collect();
    let mut w = path.join("/");
    if query {
        w.push_str("?<redacted>");
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_query_strings_and_private_links() {
        let url = "https://api-v2.soundcloud.com/media/1/stream?client_id=abc&track_authorization=xyz";
        assert_eq!(redact(url), "https://api-v2.soundcloud.com/media/1/stream?<redacted>");
        assert_eq!(
            redact("https://soundcloud.com/a/b/s-Secret1"),
            "https://soundcloud.com/a/b/s-<redacted>"
        );
    }

    #[test]
    fn strips_tokens_after_oauth_markers() {
        assert_eq!(
            redact("Authorization: OAuth 2-abc-def"),
            "Authorization: OAuth <redacted>"
        );
        assert_eq!(
            redact("Authorization:OAuth 2-abc-def next"),
            "Authorization:OAuth <redacted> next"
        );
        assert_eq!(redact("cookie oauth_token=2-abc"), "cookie oauth_token=<redacted>");
    }

    #[test]
    fn redaction_reaches_nested_values() {
        let l = line(
            "http",
            json!({
                "outer": { "url": "https://x/a?client_id=SECRET", "list": ["OAuth SECRET", { "deep": "https://x/b/s-SECRET" }] },
                "track_authorization": "SECRET",
                "headers": { "Authorization": "SECRET" },
                "attempts": [1, 2],
            }),
        );
        assert!(!l.contains("SECRET"), "{l}");
        assert!(l.contains("\"attempts\":[1,2]"), "{l}");
    }

    #[test]
    fn leaves_ordinary_messages_alone() {
        assert_eq!(redact("HTTP 503 after 3 tries"), "HTTP 503 after 3 tries");
        assert_eq!(redact("is this song ok?"), "is this song ok?");
    }

    #[test]
    fn log_lines_never_carry_a_secret() {
        let l = line(
            "http",
            json!({ "url": "https://x/stream?client_id=SECRET", "error": "OAuth SECRET", "status": 200 }),
        );
        assert!(!l.contains("SECRET"), "{l}");
        assert!(l.contains("\"status\":200"));
    }
}
