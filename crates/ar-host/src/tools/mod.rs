//! Host tools. Built-in tools and tools of mounted MCP servers implement the same [`Tool`] trait,
//! so the gate (level check, policy, approval, audit, result budget) treats them identically.

pub mod clipboard;
pub mod command;
pub mod files;
pub mod input;
pub mod jobs;
pub mod screen;
pub mod shell;
pub mod system;
pub mod ui;

use std::sync::Arc;

use anyhow::Result;
use ar_core::{Level, config::Action};
use async_trait::async_trait;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, ToolAnnotations};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::session::ToolCtx;

/// Static description of a tool plus the host-side rules that apply to it.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Arc<JsonObject>,
    /// Minimum caller level.
    pub level: Level,
    /// What the gate does when no policy rule matches.
    pub default_action: Action,
    /// Hints for clients; never the authority (the gate is).
    pub read_only: bool,
    pub destructive: bool,
}

impl ToolSpec {
    /// A spec whose input schema is derived from `P`.
    pub fn new<P: JsonSchema + 'static>(name: &str, level: Level, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: schema_for::<P>(),
            level,
            default_action: if level >= Level::Admin { Action::Ask } else { Action::Allow },
            read_only: level == Level::Observe,
            destructive: level != Level::Observe,
        }
    }

    pub fn ask(mut self) -> Self {
        self.default_action = Action::Ask;
        self
    }

    /// Runs without asking even at admin level: for tools that only read state, or undo what an
    /// approved call did.
    pub fn allow(mut self) -> Self {
        self.default_action = Action::Allow;
        self
    }

    /// Hints that the tool changes nothing on the host.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self.destructive = false;
        self
    }

    pub fn non_destructive(mut self) -> Self {
        self.destructive = false;
        self
    }

    /// The MCP tool definition shown to controllers.
    pub fn to_mcp(&self) -> rmcp::model::Tool {
        let mut annotations = ToolAnnotations::new().read_only(self.read_only).destructive(self.destructive);
        if self.read_only {
            annotations = annotations.idempotent(true);
        }
        let description = format!("{} [requires level: {}]", self.description, self.level);
        rmcp::model::Tool::new(self.name.clone(), description, self.input_schema.clone()).annotate(annotations)
    }
}

/// One capability the host offers.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;

    /// Arguments whose values may hold secrets (environment values, standard input, clipboard
    /// text). The gate shows only their size in approval prompts and the audit log; the audit's
    /// argument hash still covers them. A trait method rather than a [`ToolSpec`] field so tool
    /// specs built field by field (mounted tools) need not change.
    fn secret_args(&self) -> &'static [&'static str] {
        &[]
    }

    /// Runs the tool. An `Err` is shown to the caller as a tool error (`isError: true`) with the
    /// error's text, so messages should tell the agent what went wrong and what to do instead.
    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult>;
}

/// `args` as shown to the host owner: the values of `secret` arguments are replaced by their
/// size. In an object (an environment map) the keys stay visible and `null` (remove) stays `null`.
pub fn shown_args(args: &JsonObject, secret: &[&str]) -> JsonObject {
    fn hide(v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match v {
            Value::Null => Value::Null,
            Value::String(s) => Value::String(format!("<hidden: {} chars>", s.chars().count())),
            Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), hide(v))).collect()),
            _ => Value::String("<hidden>".into()),
        }
    }
    let mut shown = args.clone();
    for key in secret {
        if let Some(v) = shown.get_mut(*key) {
            *v = hide(v);
        }
    }
    shown
}

/// All tools of this host, in display order.
#[derive(Clone, Default)]
pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    /// The tools built into the host binary.
    pub fn builtin() -> Self {
        let mut r = Self::default();
        for t in system::tools()
            .into_iter()
            .chain(screen::tools())
            .chain(input::tools())
            .chain(ui::tools())
            .chain(clipboard::tools())
            .chain(shell::tools())
            .chain(jobs::tools())
            .chain(files::tools())
        {
            r.add(t);
        }
        r
    }

    /// Adds a tool; a later tool with the same name replaces the earlier one.
    pub fn add(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name.clone();
        self.tools.retain(|t| t.spec().name != name);
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.spec().name == name).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }
}

/// JSON schema (object root) for a parameter type, in the dialect MCP clients expect.
pub fn schema_for<P: JsonSchema + 'static>() -> Arc<JsonObject> {
    rmcp::handler::server::common::schema_for_type::<P>()
}

/// Parses tool arguments into `P`, with an error message an agent can act on.
pub fn parse<P: DeserializeOwned>(args: JsonObject) -> Result<P> {
    serde_json::from_value(serde_json::Value::Object(args)).map_err(|e| anyhow::anyhow!("invalid arguments: {e}"))
}

/// A successful result with one text block.
pub fn text(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(s.into())])
}

/// A successful result with a JSON value rendered as text plus structured content.
pub fn json(value: serde_json::Value) -> CallToolResult {
    let mut r =
        CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&value).unwrap_or_default())]);
    r.structured_content = Some(value);
    r
}

/// Parameters for tools that take none.
#[derive(Debug, Default, serde::Deserialize, JsonSchema)]
pub struct NoParams {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn shown_args_hide_secret_values_only() {
        let args = json!({
            "command": "deploy",
            "env": { "TOKEN": "s3cr3t", "OLD": null },
            "stdin": "pässword",
            "text": 5,
        });
        let shown = shown_args(args.as_object().unwrap(), &["env", "stdin", "text", "absent"]);
        assert_eq!(
            serde_json::Value::Object(shown),
            json!({
                "command": "deploy",
                "env": { "TOKEN": "<hidden: 6 chars>", "OLD": null },
                "stdin": "<hidden: 8 chars>",
                "text": "<hidden>",
            })
        );
    }

    #[test]
    fn secret_tool_arguments_are_declared() {
        let registry = Registry::builtin();
        let secret = |name: &str| registry.get(name).unwrap().secret_args().to_vec();
        assert_eq!(secret("shell"), ["env", "stdin"]);
        assert_eq!(secret("jobs_start"), ["env"]);
        assert_eq!(secret("clipboard_write"), ["text"]);
    }
}
