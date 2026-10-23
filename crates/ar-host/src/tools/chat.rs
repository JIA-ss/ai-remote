//! Coding agents on this host (see [`crate::chat`]): which are installed, the sessions, starting
//! one and getting a ticket to its conversation.
//!
//! All need level admin: an agent started here runs with the user's rights and does not ask before
//! acting. `chat_start` asks the owner by default (once per session: the messages that follow do not
//! ask again); listing, watching and stopping do not. `chat_start` and `chat_attach` return a
//! ticket for the chat channel (`app/ai-remote/chat/1`), where the conversation happens; a
//! controller that offers its tools to an AI agent does not list them, like the file transfers.
//! `chat_file` previews a file of a session (see [`crate::chat::preview`]): it returns a ticket for
//! the files channel, as `files_pull` does, without asking.

use std::sync::Arc;

use anyhow::Result;
use ar_core::Level;
use async_trait::async_trait;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::{Tool, ToolSpec, json, parse};
use crate::{
    chat::{Role, TICKET_TTL},
    session::ToolCtx,
    tools::files::{Direction, Held, TICKET_TTL as FILE_TICKET_TTL},
};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Runtimes::new()),
        Arc::new(Sessions::new()),
        Arc::new(Start::new()),
        Arc::new(Attach::new()),
        Arc::new(Update::new()),
        Arc::new(Interrupt::new()),
        Arc::new(End::new()),
        Arc::new(FilePreview::new()),
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SessionsParams {
    /// List the archived sessions instead of the others.
    #[serde(default)]
    archived: bool,
}

struct Sessions {
    spec: ToolSpec,
}

impl Sessions {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<SessionsParams>(
                "chat_sessions",
                Level::Admin,
                "The coding-agent sessions on this host, newest first: agent, folder, state (starting, idle, running, \
                 interrupting, parked, ended), who started it, how many turns, its title, git branch, what waits for \
                 an answer (pending), pinned. Not the conversation itself. Archived sessions only with archived=true.",
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

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: SessionsParams = parse(args)?;
        Ok(json(json!({"sessions": ctx.host.chat().list(Some(p.archived)), "live": ctx.host.chat().live_count()})))
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
    /// Claude Code only: the permission mode it starts in (default, acceptEdits, plan,
    /// bypassPermissions). Without one it does not ask before acting (bypassPermissions).
    #[serde(default)]
    permission_mode: Option<String>,
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
        let chat = ctx
            .host
            .chat()
            .start(ctx.session.peer.clone(), &p.runtime, &p.cwd, p.model, p.hardened, p.prompt, p.permission_mode)
            .await?;
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
                "A ticket to a coding-agent session on the chat channel (watch it and send it messages), valid once \
                 for 2 minutes.",
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
        // Every device with level admin may talk to every session: it could start an agent of its
        // own with the same rights, and each message carries who sent it.
        let role = Role::Rw;
        let ticket = ctx.host.chat().issue_ticket(&chat.id, ctx.session.peer.principal.clone(), role);
        let mut v = chat.describe();
        v["session"] = json!(chat.id);
        v["ticket"] = json!(ticket);
        v["role"] = json!(role);
        v["expires_in_secs"] = json!(TICKET_TTL.as_secs());
        Ok(json(v))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UpdateParams {
    /// The session id (from chat_sessions or chat_start).
    session: String,
    /// A new title (at most 200 characters); an empty one goes back to the agent's title or the first
    /// message.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    pinned: Option<bool>,
    /// Archived sessions are listed only by chat_sessions with archived=true.
    #[serde(default)]
    archived: Option<bool>,
}

struct Update {
    spec: ToolSpec,
}

impl Update {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<UpdateParams>(
                "chat_update",
                Level::Admin,
                "Renames, pins or archives a coding-agent session; returns the session as chat_sessions lists it.",
            )
            .allow()
            .non_destructive(),
        }
    }
}

#[async_trait]
impl Tool for Update {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: UpdateParams = parse(args)?;
        Ok(json(ctx.host.chat().update(&p.session, p.title, p.pinned, p.archived, &ctx.session.peer)?))
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

#[derive(Debug, Deserialize, JsonSchema)]
struct FileParams {
    /// The session id (from chat_sessions or chat_start).
    session: String,
    /// The file as the session names it: absolute, or relative to the session's folder.
    path: String,
}

struct FilePreview {
    spec: ToolSpec,
}

impl FilePreview {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<FileParams>(
                "chat_file",
                Level::Admin,
                "A one-time ticket to read an HTML, Markdown or image file of a coding-agent session for a preview \
                 (at most 16 MiB, inside the session's folder or named by the agent in the session), redeemed on the \
                 files channel within 2 minutes.",
            )
            .allow()
            .read_only(),
        }
    }
}

#[async_trait]
impl Tool for FilePreview {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: FileParams = parse(args)?;
        let chat = ctx.host.chat().get(&p.session)?;
        let found = tokio::task::spawn_blocking(move || chat.previewable(&p.path))
            .await
            .map_err(|e| anyhow::anyhow!("file worker failed: {e}"))??;
        let shown = dunce::simplified(&found.path).display().to_string();
        // Read now, with the agent's user's rights; the ticket carries these bytes.
        let bytes = ctx.host.chat().read_preview(&found.path).await?;
        let size = bytes.len();
        let ticket = ctx.host.transfers().issue(ctx, "chat_file", found.path, Direction::Bytes(Held(Arc::new(bytes))));
        // One line, as the file tools answer: the app reads the ticket from the first line.
        let value = json!({
            "ticket": ticket,
            "path": shown,
            "size": size,
            "kind": found.kind,
            "expires_in_secs": FILE_TICKET_TTL.as_secs(),
        });
        let mut r = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
        r.structured_content = Some(value);
        Ok(r)
    }
}
