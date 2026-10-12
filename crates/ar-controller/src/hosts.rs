//! What the controller remembers about each paired host beyond the transport's own pairing record:
//! the level the host granted at pairing time and when, and which host a call goes to when it names
//! none.

use std::path::Path;

use anyhow::Result;
use ar_core::{Level, store};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHost {
    /// Local name of the host (the transport's peer name).
    pub name: String,
    /// Level the host granted when pairing (the host may change it later).
    pub level: Option<Level>,
    pub paired_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KnownHosts {
    #[serde(default)]
    pub hosts: Vec<KnownHost>,
    /// The host calls go to when they name none (`ai-remote hosts default`), for the command line
    /// and MCP sessions alike; without it they need one when several hosts are paired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl KnownHosts {
    pub fn load(path: &Path) -> Result<Self> {
        store::read_json(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        store::write_json_atomic(path, self)
    }

    pub fn upsert(&mut self, host: KnownHost) {
        self.hosts.retain(|h| h.name != host.name);
        self.hosts.push(host);
    }

    pub fn remove(&mut self, name: &str) {
        self.hosts.retain(|h| h.name != name);
        if self.default.as_deref() == Some(name) {
            self.default = None;
        }
    }

    pub fn get(&self, name: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpairing_the_default_host_clears_it() {
        let mut known = KnownHosts::default();
        known.upsert(KnownHost { name: "lab".into(), level: None, paired_at: 1 });
        known.upsert(KnownHost { name: "office".into(), level: None, paired_at: 2 });
        known.default = Some("lab".into());
        known.remove("office");
        assert_eq!(known.default.as_deref(), Some("lab"));
        known.remove("lab");
        assert_eq!(known.default, None);
        // Files written before the default existed still load.
        let old: KnownHosts = serde_json::from_str(r#"{"hosts":[]}"#).unwrap();
        assert_eq!(old.default, None);
    }
}
