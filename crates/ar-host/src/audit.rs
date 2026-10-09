//! Append-only audit log: one JSON line per tool call, written before the call runs (decision)
//! and after it finishes (outcome).

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use serde::Serialize;

pub struct AuditLog {
    path: PathBuf,
    file: Mutex<File>,
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
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open audit log {}", path.display()))?;
        Ok(Self { path: path.to_path_buf(), file: Mutex::new(file) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, record: &AuditRecord<'_>) {
        let Ok(mut line) = serde_json::to_vec(record) else { return };
        line.push(b'\n');
        let mut f = self.file.lock().unwrap();
        if let Err(e) = f.write_all(&line).and_then(|_| f.flush()) {
            tracing::error!(%e, "cannot write audit log");
        }
    }
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
