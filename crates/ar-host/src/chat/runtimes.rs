//! The coding agents this host knows how to drive, as data: the command names to look for, the
//! protocol to speak and the arguments that make the agent run on its own.
//!
//! Adding an agent that speaks the Agent Client Protocol is one row here. Claude Code and Codex
//! have adapters of their own ([`super::adapters`]) because their native protocols carry more
//! (partial text, interrupts, questions) than their ACP bridges, which would also need Node.js and
//! an extra package installed.
//!
//! The owner can add entries in `chat.toml` ([`ChatConfig`]): an agent installed somewhere this
//! host does not look, or a second configuration of one (`CLAUDE_CONFIG_DIR` for another
//! account). Such an entry is a command, arguments put before ours and environment variables, on
//! top of a built-in entry whose protocol it speaks.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// How the host talks to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// `claude -p` with JSON lines in and out.
    ClaudeStreamJson,
    /// `codex app-server`: JSON-RPC over stdio.
    CodexAppServer,
    /// The Agent Client Protocol over stdio.
    Acp,
}

/// How the host tells whether an agent is signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    /// Not checked; nothing is reported.
    NotChecked,
    /// A command (these arguments) that exits 0 when the agent is signed in.
    Command(&'static [&'static str]),
    /// DeepSeek Harness: a DeepSeek API key or account in its environment or credential files.
    DeepSeek,
}

/// What an agent's adapter can do; shown to controllers so they offer only what works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Caps {
    /// Stops the current turn without ending the process.
    pub interrupt: bool,
    /// Continues an earlier conversation in a new process.
    pub resume: bool,
    /// Streams text as it is written (otherwise it arrives per message).
    pub partial: bool,
    /// Reports its reasoning.
    pub thinking: bool,
    /// Asks the user questions mid-turn.
    pub questions: bool,
    /// Takes a message while a turn runs into that turn (otherwise it waits for the next one).
    pub steer: bool,
    /// May take images with a message. For ACP agents this is a guess until the agent says
    /// (`promptCapabilities.image`); the session then reports what holds (an `agent` event).
    pub images: bool,
}

/// A built-in agent.
#[derive(Debug)]
pub struct Runtime {
    pub id: &'static str,
    pub name: &'static str,
    /// Command names to look for, preferred first.
    pub commands: &'static [&'static str],
    pub protocol: Protocol,
    /// Arguments that start the agent's protocol mode (ACP agents).
    pub args: &'static [&'static str],
    pub login: Login,
    /// Oldest version known to speak the protocol as used here.
    pub min_version: Option<&'static str>,
    /// ACP: the session mode that runs without asking, selected when the agent offers it.
    pub auto_mode: Option<&'static str>,
    pub caps: Caps,
}

const ACP_CAPS: Caps =
    Caps { interrupt: true, resume: true, partial: true, thinking: true, questions: false, steer: false, images: true };

/// Every agent this host knows, in display order.
pub const RUNTIMES: &[Runtime] = &[
    Runtime {
        id: "claude",
        name: "Claude Code",
        commands: &["claude"],
        protocol: Protocol::ClaudeStreamJson,
        args: &[],
        login: Login::Command(&["auth", "status"]),
        min_version: Some("2.0.0"),
        auto_mode: None,
        caps: Caps {
            interrupt: true,
            resume: true,
            partial: true,
            thinking: true,
            questions: false,
            steer: true,
            images: true,
        },
    },
    Runtime {
        id: "codex",
        name: "Codex",
        commands: &["codex"],
        protocol: Protocol::CodexAppServer,
        args: &[],
        login: Login::Command(&["login", "status"]),
        min_version: Some("0.100.0"),
        auto_mode: None,
        caps: Caps {
            interrupt: true,
            resume: true,
            partial: true,
            thinking: true,
            questions: true,
            steer: true,
            images: true,
        },
    },
    Runtime {
        id: "deepseek",
        name: "DeepSeek Harness",
        commands: &["dsh"],
        protocol: Protocol::Acp,
        args: &["--profile", "acp"],
        login: Login::DeepSeek,
        min_version: None,
        auto_mode: None,
        // `session/resume`, `usage_update`; whole messages rather than streamed text, no plans.
        caps: Caps {
            interrupt: true,
            resume: true,
            partial: false,
            thinking: true,
            questions: false,
            steer: false,
            images: false,
        },
    },
    Runtime {
        id: "gemini",
        name: "Gemini CLI",
        commands: &["gemini"],
        protocol: Protocol::Acp,
        args: &["--experimental-acp"],
        login: Login::NotChecked,
        min_version: None,
        auto_mode: Some("yolo"),
        caps: ACP_CAPS,
    },
    Runtime {
        id: "opencode",
        name: "OpenCode",
        commands: &["opencode"],
        protocol: Protocol::Acp,
        args: &["acp"],
        login: Login::NotChecked,
        min_version: None,
        auto_mode: None,
        caps: ACP_CAPS,
    },
    Runtime {
        id: "qwen",
        name: "Qwen Code",
        commands: &["qwen"],
        protocol: Protocol::Acp,
        args: &["--acp"],
        login: Login::NotChecked,
        min_version: None,
        auto_mode: Some("yolo"),
        caps: ACP_CAPS,
    },
    Runtime {
        id: "copilot",
        name: "GitHub Copilot CLI",
        commands: &["copilot"],
        protocol: Protocol::Acp,
        args: &["--acp", "--stdio"],
        login: Login::NotChecked,
        min_version: None,
        auto_mode: None,
        caps: ACP_CAPS,
    },
    Runtime {
        id: "cursor",
        name: "Cursor Agent",
        commands: &["cursor-agent", "agent"],
        protocol: Protocol::Acp,
        args: &["acp"],
        login: Login::NotChecked,
        min_version: None,
        auto_mode: None,
        caps: ACP_CAPS,
    },
    Runtime {
        id: "kimi",
        name: "Kimi CLI",
        commands: &["kimi"],
        protocol: Protocol::Acp,
        args: &["acp"],
        login: Login::NotChecked,
        min_version: None,
        auto_mode: None,
        caps: ACP_CAPS,
    },
    Runtime {
        id: "goose",
        name: "Goose",
        commands: &["goose"],
        protocol: Protocol::Acp,
        args: &["acp"],
        login: Login::NotChecked,
        min_version: None,
        auto_mode: None,
        caps: ACP_CAPS,
    },
];

pub fn builtin(id: &str) -> Option<&'static Runtime> {
    RUNTIMES.iter().find(|r| r.id == id)
}

/// `chat.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChatConfig {
    /// Whether controllers may start agents here at all. Only someone at this computer changes it
    /// (`ai-remote chat enable|disable`); it holds for bypassed devices too.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Agents the owner adds or configures.
    #[serde(default, rename = "agent", skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<CustomAgent>,
}

fn yes() -> bool {
    true
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self { enabled: true, agents: Vec::new() }
    }
}

/// An agent the owner configured in `chat.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomAgent {
    /// Its id in lists and `chat_start` (letters, digits, `-` and `_`).
    pub id: String,
    /// The built-in agent whose protocol it speaks (`claude`, `codex`, an ACP agent).
    pub base: String,
    #[serde(default)]
    pub name: Option<String>,
    /// The program: a name looked up like the built-in ones, or a path (`~` allowed). Default:
    /// the base agent's command.
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments put before the ones the host adds.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables for the agent (`~` at the start of a value is the home directory).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// `false`: messages sent while a turn runs wait for the next turn, even where the base agent
    /// could take them into the running one (an older version that cannot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer: Option<bool>,
}

impl CustomAgent {
    /// The base agent's caps as this entry narrows them.
    pub fn caps(&self, base: &Runtime) -> Caps {
        Caps { steer: base.caps.steer && self.steer != Some(false), ..base.caps }
    }
}

impl ChatConfig {
    /// Reads `chat.toml`; a missing file is the default (enabled, no custom agents).
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let cfg: Self = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        cfg.check()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        ar_core::store::write_atomic(path, text.as_bytes())
    }

    fn check(&self) -> Result<()> {
        for a in &self.agents {
            if !valid_id(&a.id) {
                bail!("chat.toml: agent id '{}' may only hold letters, digits, '-' and '_'", a.id);
            }
            if builtin(&a.id).is_some() {
                bail!("chat.toml: agent id '{}' is a built-in agent; pick another id", a.id);
            }
            if builtin(&a.base).is_none() {
                bail!("chat.toml: agent '{}' has an unknown base '{}'", a.id, a.base);
            }
        }
        Ok(())
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_valid() {
        let mut ids: Vec<_> = RUNTIMES.iter().map(|r| r.id).collect();
        assert!(ids.iter().all(|id| valid_id(id)));
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), RUNTIMES.len());
    }

    #[test]
    fn config_parses_and_rejects_bad_entries() {
        let cfg: ChatConfig = toml::from_str(
            r#"
            [[agent]]
            id = "claude-work"
            base = "claude"
            env = { CLAUDE_CONFIG_DIR = "~/.claude-work" }
            "#,
        )
        .unwrap();
        assert!(cfg.enabled);
        cfg.check().unwrap();
        let bad: ChatConfig = toml::from_str("[[agent]]\nid = \"codex\"\nbase = \"codex\"\n").unwrap();
        assert!(bad.check().is_err());
        let bad: ChatConfig = toml::from_str("[[agent]]\nid = \"x\"\nbase = \"nope\"\n").unwrap();
        assert!(bad.check().is_err());
        assert!(toml::from_str::<ChatConfig>("enabled = false\nsurprise = 1\n").is_err());
    }
}
