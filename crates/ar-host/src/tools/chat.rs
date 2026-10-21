//! Coding agents on this host (see [`crate::chat`]): which are installed, the sessions, starting
//! one and getting a ticket to its conversation.
//!
//! All need level admin: an agent started here runs with the user's rights and does not ask before
//! acting. `chat_start` asks the owner by default (once per session: the messages that follow do not
//! ask again); listing, watching and stopping do not. `chat_start` and `chat_attach` return a
//! ticket for the chat channel (`app/ai-remote/chat/1`), where the conversation happens; a
//! controller that offers its tools to an AI agent does not list them, like the file transfers.

use std::sync::Arc;

use anyhow::Result;
use ar_core::Level;
use async_trait::async_trait;
use rmcp::model::{CallToolResult, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::{NoParams, Tool, ToolSpec, json, parse};
use crate::{
    chat::{Role, TICKET_TTL},
    session::ToolCtx,
};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Runtimes::new()),
        Arc::new(Sessions::new()),
        Arc::new(Start::new()),
        Arc::new(Attach::new()),
        Arc::new(Interrupt::new()),
        Arc::new(End::new()),
    ]
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct RuntimesParams {
    /// Look again instead of using the answer of the last minute.
    #[serde(default)]
    refresh: bool,
}

struct Runtimes {
    spec: ToolSpec,
}

impl Runtimes {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<RuntimesParams>(
                "chat_runtimes",
                Level::Admin,
                "The coding agents installed on this host (Claude Code, Codex, Gemini CLI, OpenCode, ...): where each \
                 is, its version, whether it is signed in, and whether agents can start here at all (host.can_start, \
                 host.enabled).",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for Runtimes {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: RuntimesParams = parse(args)?;
        Ok(json(ctx.host.chat().runtimes(p.refresh).await))
    }
}

struct Sessions {
    spec: ToolSpec,
}

impl Sessions {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<NoParams>(
                "chat_sessions",
                Level::Admin,
                "The coding-agent sessions on this host, newest first: agent, folder, state (starting, idle, running, \
                 interrupting, parked, ended), who started it, how many turns. Not the conversation itself.",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for Sessions {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, _args: JsonObject) -> Result<CallToolResult> {
        Ok(json(json!({"sessions": ctx.host.chat().list(), "live": ctx.host.chat().live_count()})))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StartParams {
    /// The agent, by id from chat_runtimes (`claude`, `codex`, `gemini`, ...).
    runtime: String,
    /// The folder the agent works in: an absolute path on this host (`~` is the home folder).
    cwd: String,
    /// A first message, sent as soon as the agent is up.
    #[serde(default)]
    prompt: Option<String>,
    /// A model the agent should use instead of its default.
    #[serde(default)]
    model: Option<String>,
    /// Ignore the folder's own agent settings, hooks and MCP servers where the agent supports it.
    #[serde(default)]
    hardened: bool,
    /// Why, shown to the owner.
    #[serde(default)]
    intent: Option<String>,
}

struct Start {
    spec: ToolSpec,
}

impl Start {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<StartParams>(
                "chat_start",
                Level::Admin,
                "Starts a coding agent on this host in a folder, running as the signed-in user without asking before \
                 it acts (bypass mode): it may run any command and change any file the user can. Returns the session \
                 and a ticket for the chat channel (valid once, 2 minutes), over which messages go to the agent and \
                 its events come back.",
            ),
        }
    }
}

#[async_trait]
impl Tool for Start {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn secret_args(&self) -> &'static [&'static str] {
        &[]
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: StartParams = parse(args)?;
        let _ = p.intent;
        let chat =
            ctx.host.chat().start(ctx.session.peer.clone(), &p.runtime, &p.cwd, p.model, p.hardened, p.prompt).await?;
        let ticket = ctx.host.chat().issue_ticket(&chat.id, ctx.session.peer.principal.clone(), Role::Rw);
        let mut v = chat.describe();
        v["session"] = json!(chat.id);
        v["ticket"] = json!(ticket);
        v["role"] = json!(Role::Rw);
        v["expires_in_secs"] = json!(TICKET_TTL.as_secs());
        Ok(json(v))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SessionParams {
    /// The session id (from chat_sessions or chat_start).
    session: String,
}

struct Attach {
    spec: ToolSpec,
}

impl Attach {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<SessionParams>(
                "chat_attach",
                Level::Admin,
                "A ticket to watch a coding-agent session (and, for the device that started it, to keep talking), \
                 valid once for 2 minutes on the chat channel. Other devices only watch.",
            )
            .allow()
            .non_destructive(),
        }
    }
}

#[async_trait]
impl Tool for Attach {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: SessionParams = parse(args)?;
        let chat = ctx.host.chat().get(&p.session)?;
        let mine = chat.creator.principal.is_some() && chat.creator.principal == ctx.session.peer.principal;
        let role = if mine { Role::Rw } else { Role::Ro };
        let ticket = ctx.host.chat().issue_ticket(&chat.id, ctx.session.peer.principal.clone(), role);
        let mut v = chat.describe();
        v["session"] = json!(chat.id);
        v["ticket"] = json!(ticket);
        v["role"] = json!(role);
        v["expires_in_secs"] = json!(TICKET_TTL.as_secs());
        Ok(json(v))
    }
}

struct Interrupt {
    spec: ToolSpec,
}

impl Interrupt {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<SessionParams>(
                "chat_interrupt",
                Level::Admin,
                "Stops the running turn of a coding-agent session; the session stays and takes the next message.",
            )
            .allow()
            .non_destructive(),
        }
    }
}

#[async_trait]
impl Tool for Interrupt {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: SessionParams = parse(args)?;
        let chat = ctx.host.chat().get(&p.session)?;
        chat.send(crate::chat::Cmd::Interrupt { then: None });
        Ok(json(json!({"session": chat.id, "interrupting": true})))
    }
}

struct End {
    spec: ToolSpec,
}

impl End {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<SessionParams>(
                "chat_end",
                Level::Admin,
                "Ends a coding-agent session: its agent process stops; the transcript stays on the host.",
            )
            .allow(),
        }
    }
}

#[async_trait]
impl Tool for End {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: SessionParams = parse(args)?;
        let chat = ctx.host.chat().get(&p.session)?;
        chat.send(crate::chat::Cmd::End { reason: format!("ended by {}", ctx.session.peer.name) });
        Ok(json(json!({"session": chat.id, "ending": true})))
    }
}
