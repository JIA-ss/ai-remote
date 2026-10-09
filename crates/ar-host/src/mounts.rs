//! Mounted MCP servers: local MCP servers (for example an accessibility-tree driver) that the host
//! starts as child processes and exposes to its controllers, through the same gate as built-in
//! tools. Only allowlisted tools are exposed.

use ar_core::{Profile, config::HostConfig};

use crate::tools::Registry;

/// Adds the allowlisted tools of every configured mount to `registry`. Failures are logged and
/// skipped so one broken mount never stops the host.
pub async fn extend(_registry: &mut Registry, config: &HostConfig, _profile: &Profile) {
    for mount in &config.mounts {
        tracing::warn!(mount = %mount.name, "mounted servers are not implemented yet; skipping");
    }
}
