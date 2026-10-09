//! `shell`: run one command to completion with a timeout. The whole process tree is killed on
//! timeout or when the session ends.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use ar_core::Level;
use async_trait::async_trait;
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use rmcp::model::{CallToolResult, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::{Tool, ToolSpec, json, parse};
use crate::session::ToolCtx;

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Shell::new())]
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ShellParams {
    /// The command line to run.
    command: String,
    /// `powershell` (Windows default), `cmd`, or `sh` (default elsewhere).
    #[serde(default)]
    shell: Option<String>,
    /// Working directory.
    #[serde(default)]
    cwd: Option<String>,
    /// Seconds before the command is killed (default 60, max 600). Use a background job for longer work.
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// Why you are running this; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct Shell {
    spec: ToolSpec,
}

impl Shell {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<ShellParams>(
                "shell",
                Level::Admin,
                "Run a shell command on the host and return its exit code, stdout and stderr. Output is \
                 cut to its head and tail when very long. Usually needs the owner's approval.",
            )
            .ask(),
        }
    }
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: ShellParams = parse(args)?;
        let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(60).clamp(1, 600));
        let mut cmd = build_command(&p);
        if let Some(cwd) = &p.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut wrap = CommandWrap::from(cmd);
        wrap.wrap(KillOnDrop);
        #[cfg(windows)]
        {
            // Through the wrapper: JobObject sets the creation flags itself and would drop a flag
            // set on the command directly.
            wrap.wrap(process_wrap::tokio::CreationFlags(windows::Win32::System::Threading::CREATE_NO_WINDOW));
            wrap.wrap(process_wrap::tokio::JobObject);
        }
        #[cfg(unix)]
        wrap.wrap(process_wrap::tokio::ProcessGroup::leader());

        let started = std::time::Instant::now();
        let child = wrap.spawn().context("start command")?;
        // Dropping the future (timeout or session end) drops the child, which kills its whole tree.
        let output = tokio::time::timeout(timeout, Box::into_pin(child.wait_with_output())).await;
        let elapsed_ms = started.elapsed().as_millis();
        match output {
            Ok(Ok(out)) => Ok(json(json!({
                "exit_code": out.status.code(),
                "stdout": decode(&out.stdout),
                "stderr": decode(&out.stderr),
                "elapsed_ms": elapsed_ms,
                "timed_out": false,
            }))),
            Ok(Err(e)) => Err(e).context("wait for command"),
            Err(_) => Ok(json(json!({
                "exit_code": null,
                "stdout": "",
                "stderr": format!("killed after {} s; use a background job for long-running work", timeout.as_secs()),
                "elapsed_ms": elapsed_ms,
                "timed_out": true,
            }))),
        }
    }
}

fn build_command(p: &ShellParams) -> tokio::process::Command {
    let shell = p.shell.as_deref().unwrap_or(if cfg!(windows) { "powershell" } else { "sh" });
    match shell {
        #[cfg(windows)]
        "powershell" | "pwsh" => {
            // Force UTF-8 output and pass the script base64-encoded so no quoting can break it.
            let script = format!(
                "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $OutputEncoding=[Text.Encoding]::UTF8; {}",
                p.command
            );
            let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
            let exe = if shell == "pwsh" { "pwsh.exe".to_string() } else { system32("WindowsPowerShell\\v1.0\\powershell.exe") };
            let mut c = tokio::process::Command::new(exe);
            c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded]);
            c
        }
        #[cfg(windows)]
        "cmd" => {
            let mut c = tokio::process::Command::new(system32("cmd.exe"));
            c.raw_arg(format!("/d /s /c \"chcp 65001 >nul & {}\"", p.command));
            c
        }
        _ => {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", &p.command]);
            c
        }
    }
}

/// Absolute path under System32: never trust PATH for system tools.
#[cfg(windows)]
fn system32(rel: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{root}\\System32\\{rel}")
}

/// UTF-8 first; fall back to lossy decoding (legacy code pages).
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}
