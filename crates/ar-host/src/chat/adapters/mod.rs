//! Translating between a session and one agent's protocol. Each adapter runs as one task that owns
//! the agent process: it reads the agent's lines and the session's commands in one loop, so its
//! protocol state (request ids, the current turn, text already streamed) needs no locking, and
//! reports [`Out`] messages back.

mod acp;
mod claude;
mod codex;

use std::{path::Path, time::Duration};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use super::{detect::Resolved, event::Out, interrupt_grace, proc::AgentProc, runtimes::Protocol};

/// What a session asks of its adapter.
#[derive(Debug)]
pub enum LiveCmd {
    /// Start a turn with this message.
    Prompt { text: String },
    /// Stop the current turn; the adapter answers with a turn end.
    Interrupt,
    /// The answer to a question the agent asked.
    Answer { qid: String, text: String },
    /// End the process; `done` fires when it is gone.
    Shutdown { done: oneshot::Sender<()> },
}

/// How to start an agent.
pub struct Launch<'a> {
    pub resolved: &'a Resolved,
    pub cwd: &'a Path,
    pub model: Option<&'a str>,
    /// Ignore project settings, hooks and MCP servers where the agent supports that.
    pub hardened: bool,
    /// The agent's own conversation id to continue.
    pub resume: Option<&'a str>,
}

/// A running adapter.
pub struct LiveHandle {
    pub tx: mpsc::Sender<LiveCmd>,
    pub pid: u32,
    pub start: u64,
    /// How long an interrupt may take before the process is ended instead.
    pub interrupt_grace: Duration,
    /// For an agent in a runner process: where the session acknowledges what it recorded (see
    /// [`Out::Mark`]). `None` for an agent this process runs.
    pub ack: Option<mpsc::UnboundedSender<u64>>,
}

/// Starts the agent and its adapter task.
pub fn launch(l: Launch<'_>) -> Result<(LiveHandle, mpsc::Receiver<Out>)> {
    let protocol = l.resolved.runtime.protocol;
    let mut args = l.resolved.prefix.clone();
    args.extend(match protocol {
        Protocol::ClaudeStreamJson => claude::args(&l),
        Protocol::CodexAppServer => codex::args(&l),
        Protocol::Acp => acp::args(&l),
    });
    let (proc, lines) = AgentProc::spawn(&l.resolved.program, &args, l.cwd, &l.resolved.env)?;
    let (tx, cmds) = mpsc::channel(32);
    let (out_tx, out) = mpsc::channel(1024);
    let handle =
        LiveHandle { tx, pid: proc.pid, start: proc.start, interrupt_grace: interrupt_grace(protocol), ack: None };
    let name = l.resolved.name.clone();
    match protocol {
        Protocol::ClaudeStreamJson => {
            tokio::spawn(claude::run(proc, lines, cmds, out_tx, name));
        }
        Protocol::CodexAppServer => {
            let opts = codex::Opts {
                cwd: l.cwd.display().to_string(),
                model: l.model.map(str::to_string),
                resume: l.resume.map(str::to_string),
            };
            tokio::spawn(codex::run(proc, lines, cmds, out_tx, name, opts));
        }
        Protocol::Acp => {
            let opts = acp::Opts {
                cwd: l.cwd.display().to_string(),
                resume: l.resume.map(str::to_string),
                auto_mode: l.resolved.runtime.auto_mode,
                model: l.model.map(str::to_string),
            };
            tokio::spawn(acp::run(proc, lines, cmds, out_tx, name, opts));
        }
    }
    Ok((handle, out))
}

/// Sends `out`; a session that went away is not an error here (the adapter ends with it).
async fn report(tx: &mpsc::Sender<Out>, out: Out) -> bool {
    tx.send(out).await.is_ok()
}

/// Why the process ended, with the end of what it wrote to standard error.
fn exit_reason(name: &str, proc: &AgentProc, why: Option<String>) -> String {
    let tail = proc.stderr_tail();
    let tail = tail.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    match (why, tail.is_empty()) {
        (Some(w), true) => format!("{name}: {w}"),
        (Some(w), false) => format!("{name}: {w}\n{tail}"),
        (None, true) => format!("{name} exited"),
        (None, false) => format!("{name} exited:\n{tail}"),
    }
}

/// Text of a tool result as agents send it: a string, or a list of text blocks.
fn content_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()).or_else(|| b.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
