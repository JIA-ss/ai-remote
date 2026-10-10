//! Freshness persistence: `last_confirmed` — the last instant this
//! node validated the installed roster as current via an authenticated channel — lives in a tiny
//! sidecar `<config_dir>/roster.confirmed` (one epoch-seconds integer, atomic). Per-node liveness
//! state, NOT a roster-document field (keeps roster.json a pure re-serialization).
use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct FreshnessStore {
    path: PathBuf,
}

impl FreshnessStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    /// Load the persisted `last_confirmed`, or `None` when the sidecar is absent (a fresh node, or an
    /// roster installed by a build pre-dating freshness — the daemon applies the one-time upgrade grace).
    pub fn load(&self) -> Result<Option<i64>> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => Ok(s.trim().parse::<i64>().ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", self.path.display())),
        }
    }
    /// Persist `last_confirmed = epoch` atomically (temp + rename; per-call-unique temp).
    pub fn store(&self, epoch: i64) -> Result<()> {
        crate::roster::atomic_write_str(&self.path, &epoch.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trips_and_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = FreshnessStore::new(dir.path().join("roster.confirmed"));
        assert!(store.load().unwrap().is_none()); // absent → None
        store.store(1_760_000_000).unwrap();
        assert_eq!(store.load().unwrap(), Some(1_760_000_000));
    }
}
