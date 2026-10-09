//! Curated mount profiles shipped with ai-remote, as data (`crates/ar-host/profiles/*.toml`).
//!
//! A profile is a reviewed allowlist for one known MCP server: which of its tools are exposed, at
//! which level, which ask the owner first, shorter descriptions (some servers' full tool lists run
//! to hundreds of kilobytes of context), and environment settings that switch off telemetry and
//! self-update checks. `ai-remote mount add-profile <id> --command <path>` turns one into a
//! `[[mount]]` entry; the owner can still edit it afterwards.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow};
use ar_core::config::{MountConfig, MountStart, MountTool};
use serde::Deserialize;

/// (id, file contents) of every bundled profile.
const PROFILES: &[(&str, &str)] = &[("cua-driver", include_str!("../../profiles/cua-driver.toml"))];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountProfile {
    /// Default mount name (the tool-name prefix).
    pub name: String,
    /// One line about the server, shown by `mount add-profile`.
    pub about: String,
    /// Upstream version whose tool list this profile was checked against.
    pub checked_version: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub start: MountStart,
    #[serde(default)]
    pub needs_desktop: bool,
    pub tools: BTreeMap<String, MountTool>,
}

/// Ids of the bundled profiles.
pub fn ids() -> impl Iterator<Item = &'static str> {
    PROFILES.iter().map(|(id, _)| *id)
}

pub fn get(id: &str) -> Result<MountProfile> {
    let (_, text) = PROFILES
        .iter()
        .find(|(p, _)| *p == id)
        .ok_or_else(|| anyhow!("no bundled profile '{id}'; available: {}", ids().collect::<Vec<_>>().join(", ")))?;
    toml::from_str(text).with_context(|| format!("bundled profile '{id}' is invalid"))
}

impl MountProfile {
    /// The `[[mount]]` entry for a server installed at `command`.
    pub fn into_config(self, name: Option<String>, command: String, sha256: Option<String>) -> MountConfig {
        MountConfig {
            name: name.unwrap_or(self.name),
            command,
            args: self.args,
            env: self.env,
            sha256,
            start: self.start,
            needs_desktop: self.needs_desktop,
            tools: self.tools,
        }
    }
}

#[cfg(test)]
mod tests {
    use ar_core::{
        Level,
        config::{Action, validate_mount_name},
    };

    use super::*;

    #[test]
    fn bundled_profiles_parse() {
        for id in ids() {
            let p = get(id).unwrap();
            validate_mount_name(&p.name).unwrap();
            assert!(!p.tools.is_empty(), "{id}: empty allowlist");
            for (tool, entry) in &p.tools {
                assert!(super::super::tool_name(&p.name, tool).len() <= super::super::TOOL_NAME_MAX);
                if let Some(d) = entry.description() {
                    assert!(d.len() <= 600, "{id}/{tool}: keep curated descriptions short");
                }
            }
        }
        assert!(get("nope").is_err());
    }

    #[test]
    fn cua_driver_profile_is_conservative() {
        let p = get("cua-driver").unwrap();
        assert_eq!(p.args, ["mcp"]);
        assert!(p.needs_desktop);
        assert_eq!(p.env.get("CUA_DRIVER_RS_TELEMETRY_ENABLED").map(String::as_str), Some("false"));
        assert_eq!(p.env.get("CUA_DRIVER_RS_UPDATE_CHECK").map(String::as_str), Some("false"));
        assert_eq!(p.tools["get_window_state"].level(), Level::Observe);
        assert_eq!(p.tools["click"].level(), Level::Control);
        assert_eq!(p.tools["launch_app"].approve(), Some(Action::Ask));
        assert_eq!(p.tools["kill_app"].level(), Level::Admin);
        assert_eq!(p.tools["kill_app"].approve(), Some(Action::Ask));
        for never in
            ["install_ffmpeg", "install_extension", "check_for_update", "set_config", "page", "replay_trajectory"]
        {
            assert!(!p.tools.contains_key(never), "{never} must not be exposed");
        }
    }
}
