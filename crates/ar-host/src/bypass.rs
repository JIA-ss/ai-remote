//! Bypass: controllers the owner of this computer trusts with everything. A covered device's calls
//! are checked as admin whatever its level, never wait for approval, and are not stopped by `deny`
//! rules; the same holds for its file transfers.
//!
//! - Set only here, at this computer: over the local control channel (`ai-remote bypass`, the desktop
//!   app's device settings), never from a controller, so a controller (or an agent on it) cannot
//!   grant it to itself.
//! - Per device (by principal), optionally until a time; kept in `<profile>/host/bypass.json`, so it
//!   survives a restart. Forgetting a device removes its entry.
//! - What still applies: a device needs a pairing and some level (`level none`, revoke and forget
//!   still cut it off); a person can still take over or pause the agent; every call is still
//!   audited, with `bypass: true` on its records.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::Result;
use ar_core::store;
use ar_net::Peer;
use serde::{Deserialize, Serialize};

/// One device the owner bypasses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The device's principal (`eid:<hex>`).
    pub principal: String,
    /// Its name on this host when it was set (for people reading the file or a list).
    pub name: String,
    /// When it was set, in milliseconds since the Unix epoch.
    pub since_ms: u64,
    /// When it ends by itself; `None` until someone turns it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
}

impl Entry {
    fn live(&self, now_ms: u64) -> bool {
        self.until_ms.is_none_or(|u| now_ms < u)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    devices: Vec<Entry>,
}

pub struct Bypass {
    path: PathBuf,
    entries: Mutex<Vec<Entry>>,
}

fn now_ms() -> u64 {
    crate::audit::now_ms() as u64
}

impl Bypass {
    pub fn open(path: &Path) -> Result<Self> {
        let stored: Stored = store::read_json(path)?;
        Ok(Self { path: path.to_path_buf(), entries: Mutex::new(stored.devices) })
    }

    /// The entries still in force (expired ones are dropped).
    pub fn list(&self) -> Vec<Entry> {
        let now = now_ms();
        self.entries.lock().unwrap().iter().filter(|e| e.live(now)).cloned().collect()
    }

    /// Whether `peer`'s calls bypass levels, policy and approval right now.
    pub fn covers(&self, peer: &Peer) -> bool {
        let Some(principal) = peer.principal.as_deref() else { return false };
        let now = now_ms();
        self.entries.lock().unwrap().iter().any(|e| e.principal == principal && e.live(now))
    }

    /// Turns bypass on for `peer` (replacing an earlier entry), until `until_ms` when given.
    pub fn set(&self, peer: &Peer, until_ms: Option<u64>) -> Result<Entry> {
        let principal = peer.principal.clone().ok_or_else(|| anyhow::anyhow!("{} has no device id", peer.name))?;
        let entry = Entry { principal, name: peer.name.clone(), since_ms: now_ms(), until_ms };
        let mut entries = self.entries.lock().unwrap();
        let now = now_ms();
        let mut next: Vec<Entry> =
            entries.iter().filter(|e| e.principal != entry.principal && e.live(now)).cloned().collect();
        next.push(entry.clone());
        self.save(&next)?;
        *entries = next;
        Ok(entry)
    }

    /// Turns bypass off for the device `principal`; whether it was on.
    pub fn clear(&self, principal: &str) -> Result<bool> {
        let mut entries = self.entries.lock().unwrap();
        let now = now_ms();
        let was = entries.iter().any(|e| e.principal == principal && e.live(now));
        let next: Vec<Entry> = entries.iter().filter(|e| e.principal != principal && e.live(now)).cloned().collect();
        if next.len() != entries.len() {
            self.save(&next)?;
            *entries = next;
        }
        Ok(was)
    }

    fn save(&self, entries: &[Entry]) -> Result<()> {
        store::write_json_atomic(&self.path, &Stored { devices: entries.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> Peer {
        Peer { name: name.into(), principal: Some(format!("eid:{name}")), user_id: None, level: None, revoked: false }
    }

    #[test]
    fn set_clear_and_keep_across_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bypass.json");
        let b = Bypass::open(&path).unwrap();
        assert!(!b.covers(&peer("laptop")));
        b.set(&peer("laptop"), None).unwrap();
        b.set(&peer("phone"), None).unwrap();
        assert!(b.covers(&peer("laptop")) && b.covers(&peer("phone")));
        assert!(!b.covers(&peer("other")));
        let reopened = Bypass::open(&path).unwrap();
        assert_eq!(reopened.list().len(), 2);
        assert!(reopened.clear("eid:laptop").unwrap());
        assert!(!reopened.clear("eid:laptop").unwrap());
        assert!(!Bypass::open(&path).unwrap().covers(&peer("laptop")));
        assert!(Bypass::open(&path).unwrap().covers(&peer("phone")));
    }

    #[test]
    fn entries_end_by_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let b = Bypass::open(&dir.path().join("bypass.json")).unwrap();
        b.set(&peer("laptop"), Some(now_ms() - 1)).unwrap();
        assert!(!b.covers(&peer("laptop")));
        assert!(b.list().is_empty());
        b.set(&peer("laptop"), Some(now_ms() + 60_000)).unwrap();
        assert!(b.covers(&peer("laptop")));
        // A device without an id is never covered.
        let nameless = Peer { principal: None, ..peer("x") };
        assert!(!b.covers(&nameless));
        assert!(b.set(&nameless, None).is_err());
    }
}
