//! What the controller remembers about each paired host beyond the transport's own pairing record:
//! the level the host granted at pairing time and when.

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
    }

    pub fn get(&self, name: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.name == name)
    }
}
