//! Translating between a session and one agent's protocol. Each adapter runs as one task that owns
//! the agent process: it reads the agent's lines and the session's commands in one loop, so its
//! protocol state (request ids, the current turn, text already streamed) needs no locking, and
//! reports [`Out`] messages back.

mod acp;
mod claude;
mod codex;

use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use super::{Image, detect::Resolved, event::Out, interrupt_grace, proc::AgentProc, runtimes::Protocol};

/// What a session asks of its adapter.
#[derive(Debug)]
pub enum LiveCmd {
    /// Start a turn with this message.
    Prompt { text: String, images: Vec<Image> },
    /// Add this message to the running turn; answered with [`Out::Steered`], [`Out::SteerLate`] or
    /// [`Out::SteerRejected`] (sent only to agents whose caps have `steer`).
    ///
    /// [`Out::Steered`]: super::event::Out::Steered
    /// [`Out::SteerLate`]: super::event::Out::SteerLate
    /// [`Out::SteerRejected`]: super::event::Out::SteerRejected
    Steer { id: String, text: String, images: Vec<Image> },
    /// Stop the current turn; the adapter answers with a turn end.
    Interrupt,
    /// The answer to a question the agent asked: `text` answers the first part, `answers` (when
    /// given) every part, by its question.
    Answer { qid: String, text: String, answers: Option<BTreeMap<String, String>> },
    /// The answer to a permission or a plan the agent asked about (see [`Ev::Permission`],
    /// [`Ev::PlanReview`]); a plan approved with `mode` switches to that mode.
    ///
    /// [`Ev::Permission`]: super::event::Ev::Permission
    /// [`Ev::PlanReview`]: super::event::Ev::PlanReview
    Permit { rid: String, decision: Decision, message: Option<String>, mode: Option<String> },
    /// Switch the permission mode (Claude Code only; the session refuses it for other agents).
    SetMode { mode: String },
    /// Use this model and effort level from now on (`None`: the agent's default). Claude Code
    /// switches models with `set_model` and answers [`Out::ModelSet`] or a `model_rejected`
    /// notice; Codex sends them with every `turn/start`. Sent only where the running process can
    /// take the change (see the session's `set_model`).
    ///
    /// [`Out::ModelSet`]: super::event::Out::ModelSet
    SetModel { model: Option<String>, effort: Option<String> },
    /// Ask the agent for a title for the conversation that starts with `description` (Claude Code
    /// only; others ignore it). Answered with a `title` event, or not at all.
    GenerateTitle { description: String },
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
    /// `resume` is another session's conversation to copy into a new one (a fork), not to continue:
    /// Claude Code's `--fork-session`, Codex's `thread/fork`. Only those two can (see
    /// [`can_fork`]).
    pub fork: bool,
    /// Claude Code's permission mode (see [`PERMISSION_MODES`]); `None` is `bypassPermissions`.
    pub permission_mode: Option<&'a str>,
    /// The effort level (Claude Code's `--effort`, Codex's `effort`); `None`: the agent's default.
    pub effort: Option<&'a str>,
}

/// What a person decided about a permission or a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Allow this once (a plan: go ahead).
    Once,
    /// Allow it for the rest of the session, as the agent suggested.
    Session,
    /// Do not (a plan: change it, with `message` saying how).
    Deny,
}

/// Claude Code's permission modes a session may use.
pub const PERMISSION_MODES: &[&str] = &["default", "acceptEdits", "plan", "bypassPermissions"];

/// Whether sessions of agents speaking `protocol` can be forked: copied into a new conversation
/// that goes on apart from the first. ACP has no way to copy a conversation.
pub fn can_fork(protocol: Protocol) -> bool {
    matches!(protocol, Protocol::ClaudeStreamJson | Protocol::CodexAppServer)
}

/// The mode a Claude Code session runs in when none was chosen (what sessions always did).
pub const DEFAULT_PERMISSION_MODE: &str = "bypassPermissions";

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
            let mode = l.permission_mode.unwrap_or(DEFAULT_PERMISSION_MODE).to_string();
            tokio::spawn(claude::run(proc, lines, cmds, out_tx, name, mode));
        }
        Protocol::CodexAppServer => {
            let opts = codex::Opts {
                cwd: l.cwd.display().to_string(),
                model: l.model.map(str::to_string),
                effort: l.effort.map(str::to_string),
                resume: l.resume.map(str::to_string),
                fork: l.fork,
            };
            tokio::spawn(codex::run(proc, lines, cmds, out_tx, name, opts));
        }
        Protocol::Acp => {
            if l.fork {
                anyhow::bail!("{} cannot fork a conversation", l.resolved.name);
            }
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
