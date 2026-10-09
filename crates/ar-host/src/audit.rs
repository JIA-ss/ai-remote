//! Append-only audit log: one JSON line per tool call, written before the call runs (decision)
//! and after it finishes (outcome), plus the start and end of every controller session.
//!
//! Every line is also published to live subscribers (the events protocol, which feeds a
//! controller's activity timeline), after a backlog of the most recent lines. The backlog is seeded
//! from the end of the file at start, so a restarted host still shows recent history.

use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::sync::broadcast;

/// Lines kept for new subscribers.
const BACKLOG: usize = 300;
/// Bytes read from the end of the file to seed the backlog at start.
const SEED_BYTES: u64 = 256 * 1024;

pub struct AuditLog {
    path: PathBuf,
    inner: Mutex<Inner>,
    live: broadcast::Sender<Arc<str>>,
}

struct Inner {
    file: File,
    recent: VecDeque<Arc<str>>,
}

#[derive(Debug, Serialize)]
pub struct AuditRecord<'a> {
    /// Milliseconds since the Unix epoch.
    pub ts: u128,
    /// `decision` (before the call) or `outcome` (after it).
    pub kind: &'a str,
    pub session: &'a str,
    pub peer: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<&'a str>,
    pub tool: &'a str,
    /// allow | approved | denied | timeout | unavailable | level | policy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<&'a str>,
    /// BLAKE3 of the canonical argument JSON (lets you match calls without storing values).
    pub args_hash: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_preview: Option<&'a str>,
    /// ok | error | cancelled
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a str>,
}

impl AuditLog {
    pub fn open(path: &Path) -> Result<Self> {
        let recent = seed_backlog(path);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open audit log {}", path.display()))?;
        let (live, _) = broadcast::channel(1024);
        Ok(Self { path: path.to_path_buf(), inner: Mutex::new(Inner { file, recent }), live })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, record: &AuditRecord<'_>) {
        if let Ok(line) = serde_json::to_string(record) {
            self.append(line);
        }
    }

    /// Records that a controller's MCP session started (`session_start`) or ended (`session_end`).
    pub fn session(&self, kind: &str, session: &str, peer: &str, principal: Option<&str>) {
        let value = serde_json::json!({
            "ts": now_ms(),
            "kind": kind,
            "session": session,
            "peer": peer,
            "principal": principal,
        });
        self.append(value.to_string());
    }

    fn append(&self, line: String) {
        let line: Arc<str> = line.into();
        let mut inner = self.inner.lock().unwrap();
        let written = inner
            .file
            .write_all(line.as_bytes())
            .and_then(|_| inner.file.write_all(b"\n"))
            .and_then(|_| inner.file.flush());
        if let Err(e) = written {
            tracing::error!(%e, "cannot write audit log");
        }
        if inner.recent.len() == BACKLOG {
            inner.recent.pop_front();
        }
        inner.recent.push_back(line.clone());
        // Having no subscriber is not an error.
        let _ = self.live.send(line);
    }

    /// The recent lines and a receiver for every later line. Taken under one lock, so no line falls
    /// between the backlog and the live feed.
    pub fn subscribe(&self) -> (Vec<Arc<str>>, broadcast::Receiver<Arc<str>>) {
        let inner = self.inner.lock().unwrap();
        (inner.recent.iter().cloned().collect(), self.live.subscribe())
    }
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// The last complete lines of the existing log (up to [`BACKLOG`]).
fn seed_backlog(path: &Path) -> VecDeque<Arc<str>> {
    let mut out = VecDeque::new();
    let Ok(mut file) = File::open(path) else { return out };
    let Ok(len) = file.seek(SeekFrom::End(0)) else { return out };
    let start = len.saturating_sub(SEED_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return out;
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return out;
    }
    let text = String::from_utf8_lossy(&bytes);
    // Reading from the middle of the file starts inside a line: drop that partial line.
    let body = if start > 0 { text.split_once('\n').map_or("", |(_, rest)| rest) } else { &text };
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        if out.len() == BACKLOG {
            out.pop_front();
        }
        out.push_back(Arc::from(line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_get_the_backlog_then_live_lines() {
        let dir = std::env::temp_dir().join(format!("ar-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "{\"old\":1}\n{\"old\":2}\n").unwrap();
        let log = AuditLog::open(&path).unwrap();
        log.session("session_start", "s1", "peer", None);
        let (backlog, mut rx) = log.subscribe();
        assert_eq!(backlog.len(), 3, "{backlog:?}");
        assert!(backlog[2].contains("session_start"));
        log.session("session_end", "s1", "peer", None);
        assert!(rx.recv().await.unwrap().contains("session_end"));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backlog_is_bounded_and_skips_a_partial_first_line() {
        let dir = std::env::temp_dir().join(format!("ar-audit-seed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let lines: String = (0..BACKLOG + 50).map(|i| format!("{{\"n\":{i}}}\n")).collect();
        std::fs::write(&path, lines).unwrap();
        let seeded = seed_backlog(&path);
        assert_eq!(seeded.len(), BACKLOG);
        assert_eq!(&*seeded[BACKLOG - 1], format!("{{\"n\":{}}}", BACKLOG + 49));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
