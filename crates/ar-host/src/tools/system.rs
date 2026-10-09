//! `system_info`: what an agent needs to know before it starts operating this machine; `set_task`:
//! what it is about to do, for the owner watching.

use std::sync::Arc;

use anyhow::Result;
use ar_core::{Level, VERSION};
use async_trait::async_trait;
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::json;

use super::{NoParams, Tool, ToolSpec, json};
use crate::{platform, session::ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(SystemInfo::new()), Arc::new(Doctor::new()), Arc::new(SetTask::new())]
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetTaskParams {
    /// The task in one or two sentences, as the user gave it ("Export the Q3 report as a PDF, delete
    /// the old drafts and share the report with Finance."). At most 500 characters.
    task: String,
}

/// `set_task`: the agent says what it is working on; the owner's app shows it above the steps.
struct SetTask {
    spec: ToolSpec,
}

impl SetTask {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<SetTaskParams>(
                "set_task",
                Level::Observe,
                "Tell the owner what you are working on before you start, in one or two sentences as the user asked \
                 it. Their app shows it above your steps. Call it again when the task changes.",
            ),
        }
    }
}

#[async_trait]
impl Tool for SetTask {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: SetTaskParams = super::parse(args)?;
        let task: String = p.task.trim().chars().filter(|c| !c.is_control()).take(500).collect();
        if task.is_empty() {
            anyhow::bail!("say what the task is");
        }
        ctx.host.audit().event(json!({
            "kind": "task",
            "session": ctx.session.id,
            "peer": ctx.session.peer.name,
            "task": task,
        }));
        Ok(json(json!({"ok": true})))
    }
}

/// `doctor`: the graded preflight (see [`crate::doctor`]), run inside the host.
struct Doctor {
    spec: ToolSpec,
}

impl Doctor {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<NoParams>(
                "doctor",
                Level::Observe,
                "Check whether this host can really be operated: interactive session, unlocked desktop, elevation, DPI \
                 awareness, relay connection, firewall, autostart, approval prompts, audit log and disk space. Each \
                 check is ok/warn/fail with a reason and a fix. Run it before a task; if anything fails, stop and \
                 report it instead of working around it.",
            ),
        }
    }
}

#[async_trait]
impl Tool for Doctor {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, _args: JsonObject) -> Result<CallToolResult> {
        let report =
            crate::doctor::run(crate::doctor::Inputs { profile: ctx.host.profile(), mesh: Some(ctx.host.mesh()) })
                .await;
        Ok(json(serde_json::to_value(report)?))
    }
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
