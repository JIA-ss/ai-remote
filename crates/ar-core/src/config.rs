//! The host configuration file (`ai-remote.toml` in the host role directory).
//!
//! ```toml
//! [host]
//! name = "office-pc"
//! approval_timeout_secs = 60
//!
//! [network]
//! relays = []            # empty: the default public relays
//!
//! [[rule]]               # evaluated in order; the first match wins
//! tool = "shell"
//! action = "ask"
//!
//! [[mount]]
//! name = "ui"
//! command = "C:/path/to/server.exe"
//! args = ["mcp"]
//! [mount.tools]
//! click = "control"
//! launch_app = { level = "control", approve = "ask" }
//! ```

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{Level, store};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default)]
    pub host: HostSection,
    #[serde(default)]
    pub network: NetworkSection,
    /// Policy rules, evaluated in order before the built-in defaults.
    #[serde(default, rename = "rule", skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PolicyRule>,
    /// Local MCP servers this host exposes to its controllers.
    #[serde(default, rename = "mount", skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MountConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSection {
    /// Display name shown to controllers; defaults to the machine name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How long an approval prompt waits before it counts as "denied".
    #[serde(default = "default_approval_timeout")]
    pub approval_timeout_secs: u64,
}

impl Default for HostSection {
    fn default() -> Self {
        Self { name: None, approval_timeout_secs: default_approval_timeout() }
    }
}

fn default_approval_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSection {
    /// Relay URLs to use instead of the default public relays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
}

/// What the gate does with a matching call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Run it.
    Allow,
    /// Ask the host owner first; deny if nobody answers in time or the desktop is locked.
    Ask,
    /// Refuse it.
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Tool name, `*` wildcards allowed (`shell`, `ui_*`, `*`).
    pub tool: String,
    pub action: Action,
    /// Only for this caller (`eid:...`, `b64u:...` or a paired nickname).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountConfig {
    /// Prefix for the mount's tools: `<name>_<tool>`.
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Expected SHA-256 of `command`, checked before every start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Allowlist: only these tools are exposed; anything else the server offers stays hidden.
    #[serde(default)]
    pub tools: BTreeMap<String, MountTool>,
}

/// Either a bare level (`click = "control"`) or a table with more settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MountTool {
    Level(Level),
    Detailed {
        level: Level,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approve: Option<Action>,
        /// Replaces the server's own description (keeps the agent's context small).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl MountTool {
    pub fn level(&self) -> Level {
        match self {
            MountTool::Level(l) => *l,
            MountTool::Detailed { level, .. } => *level,
        }
    }

    pub fn approve(&self) -> Option<Action> {
        match self {
            MountTool::Level(_) => None,
            MountTool::Detailed { approve, .. } => *approve,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            MountTool::Level(_) => None,
            MountTool::Detailed { description, .. } => description.as_deref(),
        }
    }
}

impl HostConfig {
    /// Loads the file, or the defaults when it does not exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        store::write_atomic(path, toml::to_string_pretty(self)?.as_bytes())
    }
}

/// Matches `name` against a pattern where `*` matches any run of characters.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn rec(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => rec(&p[1..], n) || (!n.is_empty() && rec(p, &n[1..])),
            (Some(a), Some(b)) if a == b => rec(&p[1..], &n[1..]),
            _ => false,
        }
    }
    rec(pattern.as_bytes(), name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("ui_*", "ui_click"));
        assert!(!glob_match("ui_*", "shell"));
        assert!(glob_match("shell", "shell"));
        assert!(glob_match("*_window*", "focus_window"));
    }

    #[test]
    fn parses_example() {
        let cfg: HostConfig = toml::from_str(
            r#"
            [host]
            name = "office-pc"
            [[rule]]
            tool = "shell"
            action = "ask"
            [[mount]]
            name = "ui"
            command = "server.exe"
            args = ["mcp"]
            [mount.tools]
            click = "control"
            launch_app = { level = "control", approve = "ask" }
            "#,
        )
        .unwrap();
        assert_eq!(cfg.host.name.as_deref(), Some("office-pc"));
        assert_eq!(cfg.host.approval_timeout_secs, 60);
        assert_eq!(cfg.rules[0].action, Action::Ask);
        let tools = &cfg.mounts[0].tools;
        assert_eq!(tools["click"].level(), Level::Control);
        assert_eq!(tools["launch_app"].approve(), Some(Action::Ask));
    }
}
