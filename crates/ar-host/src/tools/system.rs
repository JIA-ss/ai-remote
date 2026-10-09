//! `system_info`: what an agent needs to know before it starts operating this machine.

use std::sync::Arc;

use anyhow::Result;
use ar_core::{Level, VERSION};
use async_trait::async_trait;
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::json;

use super::{NoParams, Tool, ToolSpec, json};
use crate::{platform, session::ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(SystemInfo::new())]
}

struct SystemInfo {
    spec: ToolSpec,
}

impl SystemInfo {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<NoParams>(
                "system_info",
                Level::Observe,
                "Facts about the host: OS, machine and user name, monitors, whether the desktop is usable \
                 (unlocked interactive session), and your own access level. Call this first.",
            ),
        }
    }
}

#[async_trait]
impl Tool for SystemInfo {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, _args: JsonObject) -> Result<CallToolResult> {
        let monitors = tokio::task::spawn_blocking(monitor_list).await.unwrap_or_default();
        let session = platform::session_state();
        let level = ctx.host.current_level(&ctx.session.peer).await;
        Ok(json(json!({
            "host": ctx.host.name(),
            "ai_remote_version": VERSION,
            "os": {
                "family": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "name": sysinfo::System::name(),
                "version": sysinfo::System::os_version(),
                "long_version": sysinfo::System::long_os_version(),
            },
            "machine": sysinfo::System::host_name(),
            "user": std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok(),
            "uptime_secs": sysinfo::System::uptime(),
            "session": session,
            "monitors": monitors,
            "you": {
                "peer_name": ctx.session.peer.name,
                "level": level,
            },
        })))
    }
}

fn monitor_list() -> Vec<serde_json::Value> {
    let Ok(monitors) = xcap::Monitor::all() else { return Vec::new() };
    monitors
        .iter()
        .enumerate()
        .map(|(index, m)| {
            json!({
                "index": index,
                "name": m.name().ok(),
                "primary": m.is_primary().ok(),
                "x": m.x().ok(),
                "y": m.y().ok(),
                "width": m.width().ok(),
                "height": m.height().ok(),
                "scale_factor": m.scale_factor().ok(),
            })
        })
        .collect()
}
