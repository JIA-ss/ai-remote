//! `process_list` and `process_kill`: the host's process table, and stopping processes no job of
//! this host owns (a program the user started, a stuck tool, a game that will not quit).
//!
//! - Listing reads the process table once (name, executable, parent, start time, memory). Nothing
//!   about command lines or environments: those can hold secrets, and `shell` can read them when
//!   the owner allows it.
//! - Killing pins each process by pid *and* start time, as `jobs_kill` does ([`super::jobs`]), so a
//!   pid reused between the lookup and the kill never hits an unrelated process. With `tree` (the
//!   default) the processes the target started are found through their parent links (on Unix also
//!   their session) and stopped too.
//! - Never the host itself, its supervisor, or the system's own idle and kernel processes: a tool
//!   that kills the host would end the session that called it, and the owner would have to restart
//!   it by hand.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use ar_core::Level;
use async_trait::async_trait;
use rmcp::model::{CallToolResult, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::{
    Tool, ToolSpec,
    jobs::{ProcTable, Root, wait_tree_empty},
    json, parse,
};
use crate::session::ToolCtx;

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(ProcessList::new()), Arc::new(ProcessKill::new())]
}

/// At most this many processes in one `process_list` result.
const LIST_MAX: usize = 2000;
const LIST_DEFAULT: usize = 300;
/// How long `process_kill` waits for the processes to be gone before reporting.
const KILL_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct ListParams {
    /// Only processes whose name (or executable file name) contains this, case-insensitively.
    #[serde(default)]
    name: Option<String>,
    /// Only this process.
    #[serde(default)]
    pid: Option<u32>,
    /// At most this many processes (default 300, maximum 2000), smallest pid first.
    #[serde(default)]
    limit: Option<usize>,
}

struct ProcessList {
    spec: ToolSpec,
}

impl ProcessList {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<ListParams>(
                "process_list",
                Level::Admin,
                "The host's running processes: pid, parent pid, name, executable path, start time and memory. \
                 Filter by a name substring or a pid. No command lines or environments (use shell for those). \
                 Use process_kill to stop one.",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for ProcessList {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: ListParams = parse(args)?;
        let limit = p.limit.unwrap_or(LIST_DEFAULT).clamp(1, LIST_MAX);
        let needle = p.name.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase);
        let (mut rows, total) = tokio::task::spawn_blocking(move || list(needle.as_deref(), p.pid)).await?;
        let truncated = rows.len() > limit;
        rows.truncate(limit);
        Ok(json(json!({
            "processes": rows,
            "count": rows.len(),
            "total": total,
            "truncated": truncated,
        })))
    }
}

/// The processes matching `needle` / `pid`, smallest pid first, and how many matched.
fn list(needle: Option<&str>, pid: Option<u32>) -> (Vec<serde_json::Value>, usize) {
    let mut sys = System::new();
    let kind = ProcessRefreshKind::nothing().with_memory().with_exe(UpdateKind::OnlyIfNotSet);
    match pid {
        Some(pid) => sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true, kind),
        None => sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind),
    };
    let mut rows: Vec<(u32, serde_json::Value)> = sys
        .processes()
        .values()
        .filter(|p| pid.is_none_or(|pid| p.pid().as_u32() == pid))
        .filter(|p| {
            needle.is_none_or(|n| {
                p.name().to_string_lossy().to_lowercase().contains(n)
                    || p.exe()
                        .and_then(|e| e.file_name())
                        .is_some_and(|f| f.to_string_lossy().to_lowercase().contains(n))
            })
        })
        .map(|p| {
            (
                p.pid().as_u32(),
                json!({
                    "pid": p.pid().as_u32(),
                    "parent_pid": p.parent().map(Pid::as_u32),
                    "name": p.name().to_string_lossy(),
                    "exe": p.exe().map(|e| e.to_string_lossy().into_owned()),
                    "start_time": p.start_time(),
                    "memory_bytes": p.memory(),
                }),
            )
        })
        .collect();
    rows.sort_by_key(|(pid, _)| *pid);
    let total = rows.len();
    (rows.into_iter().map(|(_, v)| v).collect(), total)
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct KillParams {
    /// The process to stop, by pid. Either this or `name`.
    #[serde(default)]
    pid: Option<u32>,
    /// Every process with exactly this name (case-insensitive; "game.exe"). Either this or `pid`.
    #[serde(default)]
    name: Option<String>,
    /// Also stop the processes it started, and theirs (default true).
    #[serde(default)]
    tree: Option<bool>,
    /// Why; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct ProcessKill {
    spec: ToolSpec,
}

impl ProcessKill {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<KillParams>(
                "process_kill",
                Level::Admin,
                "Stop a process by pid, or every process with exactly a name, and (by default) the processes it \
                 started. Waits a few seconds and reports what is still running. Never the host itself. For a \
                 background job started with jobs_start use jobs_kill.",
            ),
        }
    }
}

#[async_trait]
impl Tool for ProcessKill {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: KillParams = parse(args)?;
        let name = p.name.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase);
        let target = match (p.pid, name) {
            (Some(pid), None) => Target::Pid(pid),
            (None, Some(name)) => Target::Name(name),
            (None, None) => anyhow::bail!("pass pid or name"),
            (Some(_), Some(_)) => anyhow::bail!("pass pid or name, not both"),
        };
        let tree = p.tree.unwrap_or(true);
        let outcome = tokio::task::spawn_blocking(move || kill(&target, tree)).await??;
        Ok(json(outcome))
    }
}

enum Target {
    Pid(u32),
    Name(String),
}

/// Pids this tool never touches: the host, its supervisor (the process that started it, which would
/// restart or take the host down with it), and the system's idle and kernel processes.
fn protected(table: &ProcTable) -> Vec<u32> {
    let me = std::process::id();
    let mut out = vec![0, me];
    if cfg!(windows) {
        out.push(4);
    }
    if let Some(parent) = table.procs.iter().find(|p| p.pid == me).and_then(|p| p.parent) {
        out.push(parent);
    }
    out
}

fn kill(target: &Target, tree: bool) -> Result<serde_json::Value> {
    let table = ProcTable::read();
    let roots: Vec<_> = match target {
        Target::Pid(pid) => table.procs.iter().filter(|p| p.pid == *pid).copied().collect(),
        Target::Name(name) => table
            .procs
            .iter()
            .filter(|p| table.name(p.pid).is_some_and(|n| n.to_lowercase() == *name))
            .copied()
            .collect(),
    };
    if roots.is_empty() {
        let what = match target {
            Target::Pid(pid) => format!("no process has pid {pid}"),
            Target::Name(name) => format!("no process is named {name}"),
        };
        anyhow::bail!("{what}; process_list shows what runs");
    }
    let protected = protected(&table);
    if let Some(p) = roots.iter().find(|p| protected.contains(&p.pid)) {
        anyhow::bail!(
            "pid {} is the host itself, the process that supervises it, or a system process; stop the host with \
             its own commands instead",
            p.pid
        );
    }
    let roots: Vec<Root> = roots.iter().map(|p| Root { pid: p.pid, start: p.start, alive_until: None }).collect();
    let mut killed = Vec::new();
    for root in &roots {
        let members = if tree {
            table.members(root)
        } else {
            table.procs.iter().filter(|p| p.pid == root.pid).copied().collect()
        };
        for m in members {
            if protected.contains(&m.pid) {
                continue;
            }
            if table.kill(&m) {
                killed.push(json!({"pid": m.pid, "name": table.name(m.pid)}));
            }
        }
    }
    let deadline = std::time::Instant::now() + KILL_WAIT;
    let mut still_running = Vec::new();
    for root in &roots {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let gone = if tree { wait_tree_empty(root, remaining) } else { wait_gone(root, remaining) };
        if !gone {
            still_running.push(root.pid);
        }
    }
    let result = match (killed.len(), still_running.len()) {
        (0, _) => "nothing was stopped (the processes may need more rights than the host has)".to_string(),
        (n, 0) => format!("stopped {n} process{}", if n == 1 { "" } else { "es" }),
        (n, _) => format!(
            "sent a stop to {n} process{}, but some were still running after {} s",
            if n == 1 { "" } else { "es" },
            KILL_WAIT.as_secs()
        ),
    };
    Ok(json!({
        "killed": killed,
        "count": killed.len(),
        "still_running": still_running,
        "result": result,
    }))
}

/// Polls until the process `root` (pid and start time) is gone, or `timeout` passes.
fn wait_gone(root: &Root, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let table = ProcTable::read();
        if !table.procs.iter().any(|p| p.pid == root.pid && p.start.abs_diff(root.start) <= 1) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_finds_this_process_by_pid_and_name() {
        let me = std::process::id();
        let (rows, total) = list(None, Some(me));
        assert_eq!(total, 1, "{rows:?}");
        assert_eq!(rows[0]["pid"], me);
        let name = rows[0]["name"].as_str().unwrap().to_lowercase();
        let (rows, _) = list(Some(&name[..name.len().min(4)]), None);
        assert!(rows.iter().any(|r| r["pid"] == me));
        let (rows, total) = list(Some("no-such-process-name-xyz"), None);
        assert!(rows.is_empty() && total == 0);
    }

    #[test]
    fn kill_refuses_the_host_and_unknown_pids() {
        let me = std::process::id();
        let e = kill(&Target::Pid(me), true).unwrap_err().to_string();
        assert!(e.contains("host itself"), "{e}");
        let e = kill(&Target::Pid(u32::MAX - 7), true).unwrap_err().to_string();
        assert!(e.contains("no process has pid"), "{e}");
        let e = kill(&Target::Name("no-such-process-name-xyz".into()), true).unwrap_err().to_string();
        assert!(e.contains("no process is named"), "{e}");
    }

    #[tokio::test]
    async fn kill_stops_a_child_tree() {
        // A shell that starts a sleeping grandchild; killing the shell by pid with tree takes both.
        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 > NUL"]);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", "sleep 30"]);
            c
        };
        cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let child = cmd.spawn().unwrap();
        let pid = child.id().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (rows, _) = list(None, Some(pid));
        assert_eq!(rows.len(), 1, "the shell runs");
        let v = tokio::task::spawn_blocking(move || kill(&Target::Pid(pid), true)).await.unwrap().unwrap();
        assert!(v["count"].as_u64().unwrap() >= 1, "{v}");
        assert!(v["still_running"].as_array().unwrap().is_empty(), "{v}");
        let (rows, _) = list(None, Some(pid));
        assert!(rows.is_empty(), "the shell is gone");
        // The grandchild (ping / sleep) is gone too: nothing with our shell as parent remains.
        let table = ProcTable::read();
        assert!(!table.procs.iter().any(|p| p.parent == Some(pid)), "no leftover child");
    }
}
