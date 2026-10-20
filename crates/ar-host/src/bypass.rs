//! Bypass: controllers the owner of this computer trusts with everything. A covered device's calls
//! are checked as admin whatever its level, never wait for approval, and are not stopped by `deny`
//! rules; the same holds for its file transfers.
//!
//! - Set only here, at this computer: over the local control channel (`ai-remote bypass`, the desktop
//!   app's device settings), never from a controller, so a controller (or an agent on it) cannot
//!   grant it to itself.
//! - Per device (by principal), optionally until a time; kept in `<profile>/host/bypass.json`, so it
//!   survives a restart. Forgetting a device removes its entry.
//! - Or as the **default**: `ai-remote bypass default on` trusts every paired controller, those
//!   paired later included, until the owner turns one off. Turning a device off while the default
//!   is on (or taking its level away, revoking it, or the controller lowering its own level) puts it
//!   on an exception list, so the default no longer covers it; `bypass on <device>` takes it off
//!   that list again. Forgetting a device clears everything about it: pairing it again starts from
//!   the default.
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

/// A device the default does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exception {
    pub principal: String,
    pub name: String,
    pub since_ms: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Stored {
    /// Every paired controller is bypassed unless it is in `except`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    default: bool,
    #[serde(default)]
    devices: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    except: Vec<Exception>,
}

/// How a device is covered, for status output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Not bypassed.
    No,
    /// Its own entry (`bypass on <device>`).
    Device,
    /// The default covers it.
    Default,
    /// The default is on, but this device was turned off.
    Excepted,
}

impl Coverage {
    pub fn covered(self) -> bool {
        matches!(self, Coverage::Device | Coverage::Default)
    }
}

pub struct Bypass {
    path: PathBuf,
    state: Mutex<Stored>,
}

fn now_ms() -> u64 {
    crate::audit::now_ms() as u64
}

impl Bypass {
    pub fn open(path: &Path) -> Result<Self> {
        let stored: Stored = store::read_json(path)?;
        Ok(Self { path: path.to_path_buf(), state: Mutex::new(stored) })
    }

    /// The per-device entries still in force (expired ones are dropped).
    pub fn list(&self) -> Vec<Entry> {
        let now = now_ms();
        self.state.lock().unwrap().devices.iter().filter(|e| e.live(now)).cloned().collect()
    }

    /// Whether every paired controller is bypassed by default.
    pub fn default_on(&self) -> bool {
        self.state.lock().unwrap().default
    }

    /// The devices the default does not cover (empty while the default is off).
    pub fn exceptions(&self) -> Vec<Exception> {
        let s = self.state.lock().unwrap();
        if s.default { s.except.clone() } else { Vec::new() }
    }

    /// Whether `peer`'s calls bypass levels, policy and approval right now.
    pub fn covers(&self, peer: &Peer) -> bool {
        self.coverage(peer).covered()
    }

    /// How `peer` is covered right now.
    pub fn coverage(&self, peer: &Peer) -> Coverage {
        let Some(principal) = peer.principal.as_deref() else { return Coverage::No };
        let now = now_ms();
        let s = self.state.lock().unwrap();
        if s.devices.iter().any(|e| e.principal == principal && e.live(now)) {
            Coverage::Device
        } else if !s.default {
            Coverage::No
        } else if s.except.iter().any(|x| x.principal == principal) {
            Coverage::Excepted
        } else {
            Coverage::Default
        }
    }

    /// Turns bypass on for `peer` (replacing an earlier entry, and taking it off the exception
    /// list), until `until_ms` when given.
    pub fn set(&self, peer: &Peer, until_ms: Option<u64>) -> Result<Entry> {
        let principal = peer.principal.clone().ok_or_else(|| anyhow::anyhow!("{} has no device id", peer.name))?;
        let entry = Entry { principal, name: peer.name.clone(), since_ms: now_ms(), until_ms };
        let mut s = self.state.lock().unwrap();
        let mut next = s.clone();
        let now = now_ms();
        next.devices.retain(|e| e.principal != entry.principal && e.live(now));
        next.devices.push(entry.clone());
        next.except.retain(|x| x.principal != entry.principal);
        self.save(&next)?;
        *s = next;
        Ok(entry)
    }

    /// Turns bypass off for the device `principal` (named `name`): removes its entry, and while
    /// the default is on, adds it to the exception list (`except = true`) or leaves the default to
    /// cover it (`except = false`, for a device being forgotten). Returns whether it was covered.
    pub fn clear(&self, principal: &str, name: &str, except: bool) -> Result<bool> {
        let mut s = self.state.lock().unwrap();
        let now = now_ms();
        let was = s.devices.iter().any(|e| e.principal == principal && e.live(now))
            || (s.default && !s.except.iter().any(|x| x.principal == principal));
        let mut next = s.clone();
        next.devices.retain(|e| e.principal != principal && e.live(now));
        next.except.retain(|x| x.principal != principal);
        if except && next.default {
            next.except.push(Exception { principal: principal.to_string(), name: name.to_string(), since_ms: now });
        }
        if next.devices.len() != s.devices.len() || next.except != s.except {
            self.save(&next)?;
            *s = next;
        }
        Ok(was)
    }

    /// Turns the default on or off. Turning it on empties the exception list (a fresh "trust
    /// everyone"); turning it off keeps the per-device entries. Returns whether it changed.
    pub fn set_default(&self, on: bool) -> Result<bool> {
        let mut s = self.state.lock().unwrap();
        if s.default == on {
            return Ok(false);
        }
        let mut next = s.clone();
        next.default = on;
        next.except.clear();
        self.save(&next)?;
        *s = next;
        Ok(true)
    }

    fn save(&self, stored: &Stored) -> Result<()> {
        store::write_json_atomic(&self.path, stored)
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
        assert!(reopened.clear("eid:laptop", "laptop", true).unwrap());
        assert!(!reopened.clear("eid:laptop", "laptop", true).unwrap());
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
    }

    #[test]
    fn default_covers_everyone_but_the_excepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bypass.json");
        let b = Bypass::open(&path).unwrap();
        assert!(b.set_default(true).unwrap());
        assert!(!b.set_default(true).unwrap());
        assert_eq!(b.coverage(&peer("laptop")), Coverage::Default);
        assert_eq!(b.coverage(&peer("new-one")), Coverage::Default);
        // A device without an id (an unpaired caller) is never covered.
        let anon = Peer { principal: None, ..peer("anon") };
        assert_eq!(b.coverage(&anon), Coverage::No);

        // Turning one off puts it on the exception list; it stays there across restarts.
        assert!(b.clear("eid:laptop", "laptop", true).unwrap());
        assert_eq!(b.coverage(&peer("laptop")), Coverage::Excepted);
        assert!(!b.covers(&peer("laptop")));
        assert_eq!(Bypass::open(&path).unwrap().exceptions().len(), 1);
        // Forgetting does not except: a re-paired device starts from the default.
        assert!(!b.clear("eid:laptop", "laptop", false).unwrap());
        assert_eq!(b.coverage(&peer("laptop")), Coverage::Default);

        // Its own entry wins over the exception list and removes it from the list.
        b.clear("eid:phone", "phone", true).unwrap();
        b.set(&peer("phone"), None).unwrap();
        assert_eq!(b.coverage(&peer("phone")), Coverage::Device);
        assert!(b.exceptions().is_empty());

        // Default off: entries stay, the rest is uncovered; the exception list is not reported.
        b.clear("eid:tablet", "tablet", true).unwrap();
        assert!(b.set_default(false).unwrap());
        assert_eq!(b.coverage(&peer("phone")), Coverage::Device);
        assert_eq!(b.coverage(&peer("laptop")), Coverage::No);
        assert!(b.exceptions().is_empty());
        // Default on again starts with an empty exception list.
        b.set_default(true).unwrap();
        assert_eq!(b.coverage(&peer("tablet")), Coverage::Default);
    }
}
