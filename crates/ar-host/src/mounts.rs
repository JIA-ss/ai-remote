//! Mounted MCP servers: local MCP servers (for example an accessibility-tree driver) that the host
//! starts as child processes and exposes to its controllers through the same gate as built-in
//! tools.
//!
//! Third-party servers are untrusted in what they offer: only tools on the mount's allowlist are
//! exposed, named `<mount>_<tool>`, with the level and default action the allowlist gives them.
//! Tools a new upstream version adds stay hidden (and are logged) until the owner allowlists them,
//! and the server's own annotations are passed on as hints only; they never decide anything.

mod child;
pub mod profiles;

use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use ar_core::{
    Level, Profile,
    config::{HostConfig, MountConfig, MountStart, MountTool, validate_mount_name},
};
use async_trait::async_trait;
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::json;
use sha2::{Digest, Sha256};

pub use child::Mount;

use crate::{
    session::ToolCtx,
    tools::{Registry, Tool, ToolSpec},
};

/// Longest tool name MCP clients accept (`^[a-zA-Z0-9_-]{1,64}$`).
pub const TOOL_NAME_MAX: usize = 64;

/// How long a lazily started server may sit unused before it is stopped again.
const IDLE_STOP: Duration = Duration::from_secs(10 * 60);

const INTENT_DESCRIPTION: &str =
    "Why you are doing this; shown to the host owner in approval prompts and the audit log.";

/// Adds the allowlisted tools of every configured mount to `registry`. Failures are logged and
/// skipped so one broken mount never stops the host.
pub async fn extend(registry: &mut Registry, config: &HostConfig, profile: &Profile) {
    if config.mounts.is_empty() {
        return;
    }
    let base = match profile.mounts_dir() {
        Ok(dir) => dir,
        Err(e) => {
            tracing::error!("mounted servers are disabled: {e:#}");
            return;
        }
    };
    let mut names = HashSet::new();
    let mut mounts = Vec::new();
    for mc in &config.mounts {
        if let Err(e) = validate_mount_name(&mc.name) {
            tracing::error!("mount skipped: {e:#}");
            continue;
        }
        if !names.insert(mc.name.as_str()) {
            tracing::error!(mount = %mc.name, "mount skipped: another mount has the same name");
            continue;
        }
        mounts.push(Mount::new(mc.clone(), &base));
    }

    // Start them side by side so one slow server does not delay the others.
    let discovered = futures::future::join_all(mounts.into_iter().map(|m| async move {
        let offered = m.list_tools().await;
        (m, offered)
    }))
    .await;

    for (mount, offered) in discovered {
        let name = mount.name().to_string();
        let offered = match offered {
            Ok(tools) => tools,
            Err(e) => {
                tracing::error!(mount = %name, "mounted server is not available, its tools are not exposed: {e:#}");
                continue;
            }
        };
        let plan = plan(mount.config(), &offered);
        if !plan.hidden.is_empty() {
            tracing::info!(
                mount = %name,
                "{} tool(s) the server offers are not allowlisted and stay hidden: {}",
                plan.hidden.len(),
                summarize(&plan.hidden, 12)
            );
        }
        for missing in &plan.missing {
            tracing::warn!(
                mount = %name,
                "allowlisted tool '{missing}' is not offered by the server (renamed or removed in this version?)"
            );
        }
        let mut exposed = 0;
        for t in plan.tools {
            if registry.get(&t.spec.name).is_some() {
                tracing::error!(
                    mount = %name,
                    "'{}' would replace an existing tool and is not exposed; rename the mount",
                    t.spec.name
                );
                continue;
            }
            registry.add(Arc::new(MountedTool {
                spec: t.spec,
                upstream: t.upstream,
                strip_intent: t.strip_intent,
                mount: mount.clone(),
            }));
            exposed += 1;
        }
        if exposed == 0 {
            tracing::warn!(mount = %name, "no tool of this mount is exposed (empty allowlist?); not keeping it running");
            mount.stop().await;
            continue;
        }
        tracing::info!(mount = %name, exposed, hidden = plan.hidden.len(), "mounted server ready");
        if mount.config().start == MountStart::Lazy {
            mount.stop().await;
            spawn_idle_stopper(&mount);
        }
    }
}

/// Stops a lazily started server after [`IDLE_STOP`] without calls. Ends with the mount.
fn spawn_idle_stopper(mount: &Arc<Mount>) {
    let weak: Weak<Mount> = Arc::downgrade(mount);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            let Some(mount) = weak.upgrade() else { break };
            if mount.stop_if_idle(IDLE_STOP).await {
                tracing::info!(mount = %mount.name(), "stopped the idle mounted server; it starts again on the next call");
            }
        }
    });
}

/// What one mount exposes, decided from its allowlist and the tools the server offers.
#[derive(Debug, Clone)]
pub struct Plan {
    pub tools: Vec<PlannedTool>,
    /// Offered by the server but not allowlisted: hidden.
    pub hidden: Vec<String>,
    /// Allowlisted but not offered by the server.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PlannedTool {
    /// The tool's name on the mounted server.
    pub upstream: String,
    pub spec: ToolSpec,
    /// The server's schema does not declare `intent`, so it is removed before forwarding.
    pub strip_intent: bool,
}

/// Matches the allowlist against the offered tools.
pub fn plan(mount: &MountConfig, offered: &[rmcp::model::Tool]) -> Plan {
    let mut tools = Vec::new();
    let mut hidden = Vec::new();
    let mut taken = HashSet::new();
    for tool in offered {
        let upstream = tool.name.to_string();
        let Some(entry) = mount.tools.get(&upstream) else {
            hidden.push(upstream);
            continue;
        };
        let spec = spec_for(&mount.name, entry, tool);
        if !taken.insert(spec.name.clone()) {
            // Two upstream names that only differ in characters we replace.
            tracing::warn!(mount = %mount.name, "'{upstream}' maps to the tool name '{}' already in use; skipped", spec.name);
            hidden.push(upstream);
            continue;
        }
        tools.push(PlannedTool { strip_intent: !declares_intent(&tool.input_schema), upstream, spec });
    }
    let offered_names: HashSet<&str> = offered.iter().map(|t| t.name.as_ref()).collect();
    let missing = mount.tools.keys().filter(|k| !offered_names.contains(k.as_str())).cloned().collect();
    Plan { tools, hidden, missing }
}

fn spec_for(mount: &str, entry: &MountTool, tool: &rmcp::model::Tool) -> ToolSpec {
    let level = entry.level();
    let mutating = level > Level::Observe || entry.approve().is_some();
    let schema = if mutating && !declares_intent(&tool.input_schema) {
        with_intent(&tool.input_schema)
    } else {
        (*tool.input_schema).clone()
    };
    let description = entry
        .description()
        .map(str::to_string)
        .or_else(|| tool.description.as_ref().map(|d| d.to_string()))
        .unwrap_or_else(|| format!("'{}' of the mounted server '{mount}'.", tool.name));
    // Hints for clients only (the gate never reads them). MCP defaults: not read-only, destructive.
    let hints = tool.annotations.as_ref();
    let read_only = hints.and_then(|a| a.read_only_hint).unwrap_or(false);
    let destructive = !read_only && hints.and_then(|a| a.destructive_hint).unwrap_or(true);
    ToolSpec {
        name: tool_name(mount, &tool.name),
        description,
        input_schema: Arc::new(schema),
        level,
        default_action: entry.action(),
        read_only,
        destructive,
    }
}

/// `<mount>_<tool>` in the tool-name alphabet MCP clients accept: other characters become `_`,
/// and a name longer than [`TOOL_NAME_MAX`] is cut and made unique again with a short hash.
pub fn tool_name(mount: &str, tool: &str) -> String {
    let raw = format!("{mount}_{tool}");
    let mut name: String =
        raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if name.len() > TOOL_NAME_MAX {
        let hash = blake3::hash(raw.as_bytes()).to_hex();
        name.truncate(TOOL_NAME_MAX - 9);
        name.push('_');
        name.push_str(&hash[..8]);
    }
    name
}

fn declares_intent(schema: &JsonObject) -> bool {
    schema.get("properties").and_then(|p| p.get("intent")).is_some()
}

/// Adds the optional `intent` argument the gate shows in approval prompts and the audit log.
fn with_intent(schema: &JsonObject) -> JsonObject {
    let mut schema = schema.clone();
    let properties = schema.entry("properties").or_insert_with(|| json!({}));
    if let Some(p) = properties.as_object_mut() {
        p.insert("intent".into(), json!({ "type": "string", "description": INTENT_DESCRIPTION }));
    }
    schema
}

fn summarize(names: &[String], max: usize) -> String {
    let mut s = names.iter().take(max).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > max {
        s.push_str(&format!(", ... ({} more)", names.len() - max));
    }
    s
}

/// One allowlisted tool of a mounted server, forwarded to it.
struct MountedTool {
    spec: ToolSpec,
    upstream: String,
    strip_intent: bool,
    mount: Arc<Mount>,
}

#[async_trait]
impl Tool for MountedTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, mut args: JsonObject) -> Result<CallToolResult> {
        if self.mount.config().needs_desktop {
            let state = crate::platform::session_state();
            if !state.desktop_usable {
                bail!(
                    "'{}' needs the host's desktop, which is not usable right now: {}. Try again once \
                     the desktop is unlocked.",
                    self.spec.name,
                    state.problem.as_deref().unwrap_or("unknown reason")
                );
            }
        }
        if self.strip_intent {
            // The gate already read it; a strict server would reject an argument it does not know.
            args.remove("intent");
        }
        self.mount.call(&self.upstream, args).await
    }
}

/// SHA-256 of a file as lowercase hex (the form `sha256` in the config uses).
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(data_encoding::HEXLOWER.encode(&hasher.finalize()))
}

/// Where a mount's `command` points: absolute paths as written, relative paths under the profile's
/// mounts directory (where mount binaries are installed), bare program names left to the system's
/// search path.
pub fn resolve_command(command: &str, base: &Path) -> PathBuf {
    let path = Path::new(command);
    if path.is_absolute() || path.components().count() <= 1 { path.to_path_buf() } else { base.join(path) }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ar_core::config::Action;
    use rmcp::model::ToolAnnotations;

    use super::*;

    fn offered(name: &str, schema: serde_json::Value) -> rmcp::model::Tool {
        let serde_json::Value::Object(schema) = schema else { panic!("schema must be an object") };
        rmcp::model::Tool::new(name.to_string(), format!("upstream {name}"), Arc::new(schema))
    }

    fn mount(tools: &[(&str, MountTool)]) -> MountConfig {
        MountConfig {
            name: "ui".into(),
            command: "server.exe".into(),
            args: vec![],
            env: BTreeMap::new(),
            sha256: None,
            start: MountStart::Eager,
            needs_desktop: false,
            tools: tools.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        }
    }

    #[test]
    fn names_follow_the_client_alphabet() {
        assert_eq!(tool_name("ui", "click"), "ui_click");
        assert_eq!(tool_name("ui", "browser.navigate"), "ui_browser_navigate");
        assert_eq!(tool_name("web", "ö/x"), "web___x");
        let long = tool_name("ui", &"a".repeat(100));
        assert_eq!(long.len(), TOOL_NAME_MAX);
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        assert_ne!(long, tool_name("ui", &format!("{}b", "a".repeat(99))), "cut names stay unique");
    }

    #[test]
    fn only_allowlisted_tools_are_exposed() {
        let cfg = mount(&[
            ("echo", MountTool::Level(Level::Observe)),
            (
                "launch",
                MountTool::Detailed {
                    level: Level::Control,
                    approve: Some(Action::Ask),
                    description: Some("Short.".into()),
                },
            ),
            ("wipe", MountTool::Level(Level::Admin)),
            ("gone", MountTool::Level(Level::Observe)),
        ]);
        let schema = json!({"type": "object", "properties": {"text": {"type": "string"}}});
        let tools = [
            offered("echo", schema.clone()),
            offered("launch", schema.clone()),
            offered("wipe", json!({"type": "object"})),
            offered("secret", schema.clone()),
        ];
        let p = plan(&cfg, &tools);
        let names: Vec<_> = p.tools.iter().map(|t| t.spec.name.as_str()).collect();
        assert_eq!(names, ["ui_echo", "ui_launch", "ui_wipe"]);
        assert_eq!(p.hidden, ["secret"]);
        assert_eq!(p.missing, ["gone"]);

        let echo = &p.tools[0].spec;
        assert_eq!((echo.level, echo.default_action), (Level::Observe, Action::Allow));
        assert_eq!(echo.description, "upstream echo");
        assert!(!declares_intent(&echo.input_schema), "observe tools get no intent argument");

        let launch = &p.tools[1].spec;
        assert_eq!((launch.level, launch.default_action), (Level::Control, Action::Ask));
        assert_eq!(launch.description, "Short.");
        assert!(declares_intent(&launch.input_schema));
        assert!(p.tools[1].strip_intent, "the server never sees an intent it did not declare");

        let wipe = &p.tools[2].spec;
        assert_eq!((wipe.level, wipe.default_action), (Level::Admin, Action::Ask), "admin tools ask by default");
        assert!(declares_intent(&wipe.input_schema), "intent is added even without a properties map");
    }

    #[test]
    fn a_declared_intent_is_forwarded() {
        let cfg = mount(&[("note", MountTool::Level(Level::Control))]);
        let tools = [offered("note", json!({"type": "object", "properties": {"intent": {"type": "string"}}}))];
        let p = plan(&cfg, &tools);
        assert!(!p.tools[0].strip_intent);
    }

    #[test]
    fn annotations_are_hints_only() {
        let cfg = mount(&[("read", MountTool::Level(Level::Control)), ("plain", MountTool::Level(Level::Observe))]);
        let read = offered("read", json!({"type": "object"})).annotate(ToolAnnotations::new().read_only(true));
        let plain = offered("plain", json!({"type": "object"}));
        let p = plan(&cfg, &[read, plain]);
        let read = &p.tools[0].spec;
        assert!(read.read_only && !read.destructive);
        assert_eq!(read.level, Level::Control, "a read-only hint does not lower the level");
        let plain = &p.tools[1].spec;
        assert!(!plain.read_only && plain.destructive, "no annotations: MCP defaults (destructive)");
    }

    #[test]
    fn command_resolution() {
        let base = Path::new("/profile/host/mounts");
        assert_eq!(resolve_command("npx", base), PathBuf::from("npx"));
        assert_eq!(resolve_command("cua/cua-driver.exe", base), base.join("cua/cua-driver.exe"));
        let abs = if cfg!(windows) { "C:\\tools\\server.exe" } else { "/opt/server" };
        assert_eq!(resolve_command(abs, base), PathBuf::from(abs));
    }

    #[test]
    fn hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256_file(&path).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
