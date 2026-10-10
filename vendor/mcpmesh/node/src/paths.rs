//! Every filesystem location a node reads or writes, resolved ONCE at construction.
//! The node itself never consults the environment: the embedder picks a root
//! ([`NodePaths::under_root`] — layout-identical to a `mcpmesh --profile <root>` dir),
//! and the daemon shell resolves the standard per-user layout ([`NodePaths::from_env`]).
use std::path::{Path, PathBuf};

/// The resolved on-disk world of one node. Config-file overrides (`[identity].device_key`,
/// `[identity].user_key`) still win over the two key paths here, exactly as for the daemon.
#[derive(Debug, Clone)]
pub struct NodePaths {
    pub config_path: PathBuf,
    pub device_key_path: PathBuf,
    pub user_key_path: PathBuf,
    pub roster_path: PathBuf,
    pub state_db_path: PathBuf,
    /// Outstanding pairing invites (#87b) — bearer secrets, written 0600. Beside the trust store
    /// because it is durable per-node state, not config.
    pub invites_path: PathBuf,
    pub blobs_dir: PathBuf,
    pub blob_scopes_path: PathBuf,
    pub audit_dir: PathBuf,
}

impl NodePaths {
    /// The profile-root layout under one directory: `config/` (config.toml + keys + roster),
    /// `data/` (state.redb, blobs), `state/` (the audit log) — see module doc.
    pub fn under_root(root: &Path) -> Self {
        let config = root.join("config");
        let data = root.join("data");
        NodePaths {
            config_path: config.join("config.toml"),
            device_key_path: config.join("device.key"),
            user_key_path: config.join("user.key"),
            roster_path: config.join("roster.json"),
            state_db_path: data.join("state.redb"),
            invites_path: data.join("invites.json"),
            blobs_dir: data.join("blobs"),
            blob_scopes_path: data.join("blob-scopes.redb"),
            audit_dir: root.join("state").join("audit"),
        }
    }

    /// The standard per-user layout, from the same `mcpmesh_trust::paths` rules the
    /// porcelain uses (XDG/APPDATA, honoring a profile root). Daemon-shell only —
    /// an embedded node passes an explicit root instead.
    pub fn from_env() -> std::io::Result<Self> {
        use mcpmesh_trust::paths as p;
        Ok(NodePaths {
            config_path: p::default_config_path()?,
            device_key_path: p::default_device_key_path()?,
            user_key_path: p::default_user_key_path()?,
            roster_path: p::default_roster_path()?,
            state_db_path: p::default_state_db_path()?,
            invites_path: p::default_invites_path()?,
            blobs_dir: p::default_blobs_dir()?,
            blob_scopes_path: p::default_blob_scopes_path()?,
            audit_dir: p::default_audit_dir()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The one-root layout MUST equal the `--profile <root>` layout (local-api paths.rs
    /// profile-root arms): config under `<root>/config`, data under `<root>/data`, state
    /// under `<root>/state`. An embedded node's root dir is a valid CLI profile dir.
    #[test]
    fn under_root_matches_the_profile_layout() {
        let p = NodePaths::under_root(Path::new("/r"));
        assert_eq!(p.config_path, Path::new("/r/config/config.toml"));
        assert_eq!(p.device_key_path, Path::new("/r/config/device.key"));
        assert_eq!(p.user_key_path, Path::new("/r/config/user.key"));
        assert_eq!(p.roster_path, Path::new("/r/config/roster.json"));
        assert_eq!(p.state_db_path, Path::new("/r/data/state.redb"));
        assert_eq!(p.blobs_dir, Path::new("/r/data/blobs"));
        assert_eq!(p.blob_scopes_path, Path::new("/r/data/blob-scopes.redb"));
        assert_eq!(p.audit_dir, Path::new("/r/state/audit"));
    }
}
