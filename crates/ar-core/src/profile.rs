//! Per-user profile directory holding device identities and all local state.
//!
//! One profile holds up to two roles, each with its own identity: the **host** (this machine can be
//! controlled) and the **controller** (this machine controls others). The default location is the
//! platform's local, non-roaming data directory, so a roaming Windows profile never copies one
//! device identity onto several machines. `AI_REMOTE_HOME` overrides it, which lets several
//! profiles run side by side on one machine (tests, local development).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Environment variable that overrides the profile directory.
pub const HOME_ENV: &str = "AI_REMOTE_HOME";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Controller,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Host => "host",
            Role::Controller => "controller",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Profile {
    root: PathBuf,
}

impl Profile {
    /// Opens the profile from `AI_REMOTE_HOME`, or the platform default.
    pub fn open_default() -> Result<Self> {
        if let Some(dir) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Self::at(PathBuf::from(dir));
        }
        let dirs = directories::ProjectDirs::from("", "", "ai-remote")
            .context("cannot determine the user's data directory; set AI_REMOTE_HOME")?;
        Self::at(dirs.data_local_dir().to_path_buf())
    }

    /// Opens (and creates if needed) a profile at an explicit directory.
    pub fn at(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).with_context(|| format!("create profile directory {}", root.display()))?;
        // dunce keeps the plain `C:\...` form where it is equivalent: the `\\?\` form that
        // std::fs::canonicalize returns on Windows leaks into every path shown to users and
        // passed to tools, and cmd and many programs reject it.
        let root = dunce::canonicalize(&root).unwrap_or(root);
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Short stable id of this profile (16 hex digits), for names that must differ per profile on
    /// one machine: the control pipe, the autostart task, the LaunchAgent label.
    pub fn instance_id(&self) -> String {
        let key = self.root.to_string_lossy();
        // Windows paths are case-insensitive; one directory must map to one id.
        #[cfg(windows)]
        let key = key.to_lowercase();
        format!("{:016x}", fnv1a(key.as_bytes()))
    }

    /// Directory of one role, created on demand.
    pub fn role_dir(&self, role: Role) -> Result<PathBuf> {
        let dir = self.root.join(role.as_str());
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(dir)
    }

    /// The role's device secret key (hex). Treat as a password.
    pub fn identity_path(&self, role: Role) -> Result<PathBuf> {
        Ok(self.role_dir(role)?.join("identity.key"))
    }

    /// The transport node's own state (pairings, revocations, invites).
    pub fn mesh_dir(&self, role: Role) -> Result<PathBuf> {
        Ok(self.role_dir(role)?.join("mesh"))
    }

    /// Host configuration: name, policy rules, mounted MCP servers.
    pub fn host_config_path(&self) -> Result<PathBuf> {
        Ok(self.role_dir(Role::Host)?.join("ai-remote.toml"))
    }

    /// Host: append-only log of every tool call.
    pub fn audit_log_path(&self) -> Result<PathBuf> {
        Ok(self.role_dir(Role::Host)?.join("audit.jsonl"))
    }

    /// Host: approver keys accepted from paired devices.
    pub fn approvers_path(&self) -> Result<PathBuf> {
        Ok(self.role_dir(Role::Host)?.join("approvers.json"))
    }

    /// Host: background job logs and metadata.
    pub fn jobs_dir(&self) -> Result<PathBuf> {
        let dir = self.role_dir(Role::Host)?.join("jobs");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(dir)
    }

    /// Host: downloaded mount binaries (versioned).
    pub fn mounts_dir(&self) -> Result<PathBuf> {
        let dir = self.role_dir(Role::Host)?.join("mounts");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(dir)
    }

    /// Log file of a background daemon of this role.
    pub fn log_path(&self, role: Role) -> Result<PathBuf> {
        Ok(self.role_dir(role)?.join(format!("{}.log", role.as_str())))
    }

    /// Local control endpoint of a running daemon of this role: an owner-only named pipe on
    /// Windows, a 0600 Unix socket elsewhere. Unique per profile so profiles never collide.
    pub fn control_endpoint(&self, role: Role) -> Result<PathBuf> {
        #[cfg(windows)]
        {
            Ok(PathBuf::from(format!(r"\\.\pipe\ai-remote-{}-{}", role.as_str(), self.instance_id())))
        }
        #[cfg(not(windows))]
        {
            Ok(self.role_dir(role)?.join("control.sock"))
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_profile(name: &str) -> (PathBuf, Profile) {
        let dir = std::env::temp_dir().join(format!("ar-profile-test-{name}-{}", std::process::id()));
        let profile = Profile::at(&dir).unwrap();
        (dir, profile)
    }

    #[test]
    fn root_is_canonical_without_verbatim_prefix() {
        let (dir, profile) = temp_profile("root");
        let shown = profile.root().display().to_string();
        // The verbatim prefix is `\\?\` (two backslashes).
        assert!(!shown.starts_with(r"\\?\"), "{shown}");
        assert!(profile.root().is_absolute());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn instance_id_is_stable_and_per_directory() {
        let (a_dir, a) = temp_profile("id-a");
        let (b_dir, b) = temp_profile("id-b");
        assert_eq!(a.instance_id(), Profile::at(&a_dir).unwrap().instance_id());
        assert_ne!(a.instance_id(), b.instance_id());
        assert_eq!(a.instance_id().len(), 16);
        std::fs::remove_dir_all(&a_dir).unwrap();
        std::fs::remove_dir_all(&b_dir).unwrap();
    }
}
