//! `shell`: run one command to completion with a timeout.
//!
//! The command runs in its own Job Object (Windows) or process group (Unix). When the call ends
//! for any reason (normal exit, timeout, or the session ending and the gate dropping the call),
//! everything the command started is killed, so a call never leaks processes (lesson F8). The call
//! ends when the command's own process exits (plus a short grace to collect buffered output), not
//! when everything it started has: a command that leaves a process running must not hang the call
//! until its timeout. Output is read while the command runs, so a timeout still returns what was
//! printed before it.

use std::{collections::VecDeque, process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use ar_core::{Level, budget};
use async_trait::async_trait;
use process_wrap::tokio::ChildWrapper;
use rmcp::model::{CallToolResult, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::{
    Tool, ToolSpec,
    command::{self, CommandSpec, EnvMap, Shell},
    json, parse,
};
use crate::session::ToolCtx;

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(ShellTool::new())]
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ShellParams {
    /// The command line to run. PowerShell and sh accept multi-line scripts; cmd takes one line.
    command: String,
    /// `powershell` (Windows default), `pwsh`, `cmd`, or `sh` (default elsewhere), `bash`, `zsh`.
    #[serde(default)]
    shell: Option<String>,
    /// Working directory.
    #[serde(default)]
    cwd: Option<String>,
    /// Environment changes for this command: a string sets the variable, null removes it. Values
    /// are not shown in approval prompts or the audit log (names are).
    #[serde(default)]
    env: Option<EnvMap>,
    /// Text written to the command's standard input (UTF-8), which is then closed. In PowerShell
    /// read it with `[Console]::In.ReadToEnd()` or pipe it into a native program; `$input` is empty.
    /// Not shown in approval prompts or the audit log (only its length).
    #[serde(default)]
    stdin: Option<String>,
    /// Seconds before the command is killed (default 60, max 600). Use jobs_start for longer work.
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// Why you are running this; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

struct ShellTool {
    spec: ToolSpec,
}

impl ShellTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<ShellParams>(
                "shell",
                Level::Admin,
                "Run a shell command on the host and wait for it: returns exit_code, stdout and stderr (long \
                 output keeps its head and tail). The command and every process it starts are killed when the \
                 call returns or times out, so use jobs_start for servers, GUI apps or anything over ~30 s. \
                 Output is UTF-8. Usually needs the owner's approval.",
            )
            .ask(),
        }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn secret_args(&self) -> &'static [&'static str] {
        &["env", "stdin"]
    }

    async fn call(&self, _ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: ShellParams = parse(args)?;
        let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(60).clamp(1, 600));
        let spec = CommandSpec {
            command: &p.command,
            shell: Shell::parse(p.shell.as_deref())?,
            cwd: p.cwd.as_deref(),
            env: p.env.as_ref(),
        };
        let out = run(&spec, p.stdin.as_deref(), timeout).await?;
        let mut result = json!({
            "exit_code": out.exit_code,
            "stdout": budget::head_tail(&out.stdout, STREAM_LIMIT, STREAM_KEEP),
            "stderr": budget::head_tail(&out.stderr, STREAM_LIMIT, STREAM_KEEP),
            "elapsed_ms": out.elapsed_ms,
            "timed_out": out.timed_out,
        });
        if out.timed_out {
            result["note"] = json!(format!(
                "killed after {} s together with every process it started; use jobs_start for long-running work",
                timeout.as_secs()
            ));
        }
        Ok(json(result))
    }
}

/// Characters kept per stream in the result, so the JSON stays below the gate's result budget
/// (which would otherwise cut it in the middle).
const STREAM_LIMIT: usize = 24_000;
const STREAM_KEEP: usize = 11_000;
/// After the command's own process exits, how long to keep reading output still buffered in the
/// pipes (or held open by a process it left behind, which is killed when the call returns).
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Result of [`run`].
#[derive(Debug)]
pub(crate) struct RunOutput {
    /// `None` when the command was killed by the timeout.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub elapsed_ms: u128,
}

/// Runs a command to completion (or `timeout`), feeding it `stdin`. The whole process tree is
/// killed when this returns or when the returned future is dropped.
pub(crate) async fn run(spec: &CommandSpec<'_>, stdin: Option<&str>, timeout: Duration) -> Result<RunOutput> {
    let mut cmd = command::build(spec)?;
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // No console window (the background host has none to share) and the whole tree dies with the
    // child handle.
    let mut wrap = crate::platform::process_tree(cmd);

    let started = std::time::Instant::now();
    let child = wrap.spawn().with_context(|| format!("start {}", spec.shell.name()))?;
    let mut child = TreeGuard(child);

    if let (Some(text), Some(mut pipe)) = (stdin, child.0.stdin().take()) {
        let bytes = text.as_bytes().to_vec();
        // A separate task: a command that does not read its input must not block the call. Errors
        // (the command exited without reading everything) are expected and ignored.
        tokio::spawn(async move {
            let _ = pipe.write_all(&bytes).await;
            let _ = pipe.shutdown().await;
        });
    }
    let stdout_pipe = child.0.stdout().take();
    let stderr_pipe = child.0.stderr().take();
    let mut stdout = Capped::default();
    let mut stderr = Capped::default();

    let finished = tokio::time::timeout(timeout, async {
        let exit = command::main_process(&mut *child.0).wait();
        let pumps = async { tokio::join!(pump(stdout_pipe, &mut stdout), pump(stderr_pipe, &mut stderr)) };
        tokio::pin!(exit, pumps);
        tokio::select! {
            status = &mut exit => {
                let _ = tokio::time::timeout(DRAIN_GRACE, &mut pumps).await;
                status
            }
            _ = &mut pumps => exit.await,
        }
    })
    .await;
    let elapsed_ms = started.elapsed().as_millis();
    let (exit_code, timed_out) = match finished {
        Ok(status) => (status.context("wait for the command")?.code(), false),
        Err(_) => (None, true),
    };
    // Kills whatever is still running: the timed-out command, or processes it left behind.
    drop(child);
    Ok(RunOutput { exit_code, stdout: stdout.into_string(), stderr: stderr.into_string(), timed_out, elapsed_ms })
}

/// Kills the whole tree when dropped. The Job Object would do it on Windows anyway, but dropping
/// a process-group child on Unix only kills the group leader.
struct TreeGuard(Box<dyn ChildWrapper>);

impl Drop for TreeGuard {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// Reads a pipe to its end into `buf`. Cancel-safe in the sense that matters here: everything
/// read before cancellation is already in `buf`.
async fn pump<R: AsyncRead + Unpin>(pipe: Option<R>, buf: &mut Capped) {
    let Some(mut pipe) = pipe else { return };
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.push(&chunk[..n]),
        }
    }
}

/// Output buffer with bounded memory: the first [`Capped::HEAD`] bytes and the last
/// [`Capped::TAIL`] bytes, counting what was dropped in between.
#[derive(Default)]
struct Capped {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: u64,
}

impl Capped {
    const HEAD: usize = 256 * 1024;
    const TAIL: usize = 256 * 1024;

    fn push(&mut self, mut data: &[u8]) {
        if self.head.len() < Self::HEAD {
            let n = data.len().min(Self::HEAD - self.head.len());
            self.head.extend_from_slice(&data[..n]);
            data = &data[n..];
        }
        self.tail.extend(data);
        let excess = self.tail.len().saturating_sub(Self::TAIL);
        if excess > 0 {
            self.tail.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    fn into_string(self) -> String {
        let mut s = command::decode(&self.head);
        if self.dropped > 0 {
            s.push_str(&format!("\n\n[... {} bytes omitted ...]\n\n", self.dropped));
        }
        let (a, b) = self.tail.as_slices();
        if !a.is_empty() || !b.is_empty() {
            s.push_str(&command::decode(&[a, b].concat()));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(command: &'a str, shell: Option<&str>) -> CommandSpec<'a> {
        CommandSpec { command, shell: Shell::parse(shell).unwrap(), cwd: None, env: None }
    }

    #[test]
    fn capped_keeps_head_and_tail() {
        let mut c = Capped::default();
        c.push(&vec![b'a'; Capped::HEAD]);
        c.push(&[b'b'; 100]);
        c.push(&vec![b'c'; Capped::TAIL]);
        assert_eq!(c.dropped, 100);
        let s = c.into_string();
        assert!(s.starts_with('a') && s.ends_with('c'));
        assert!(s.contains("[... 100 bytes omitted ...]"));
    }

    #[tokio::test]
    async fn exit_code_stdout_stderr_and_utf8() {
        let cmd = if cfg!(windows) {
            "Write-Output 'héllo 世界'; [Console]::Error.WriteLine('oops'); cmd /c exit 3"
        } else {
            "printf 'héllo 世界\\n'; echo oops >&2; exit 3"
        };
        let out = run(&spec(cmd, None), None, Duration::from_secs(60)).await.unwrap();
        assert_eq!(out.exit_code, Some(3), "{out:?}");
        assert_eq!(out.stdout.trim_end(), "héllo 世界", "{out:?}");
        assert_eq!(out.stderr.trim_end(), "oops", "{out:?}");
        assert!(!out.timed_out);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_errors_are_plain_text_without_progress_noise() {
        let cmd = "Write-Progress -Activity busy -Status x; Write-Error 'bad thing'; Get-Item 'Z:/no/such/thing'";
        let out = run(&spec(cmd, None), None, Duration::from_secs(60)).await.unwrap();
        assert_eq!(out.exit_code, Some(1), "{out:?}");
        assert!(!out.stderr.contains("CLIXML"), "{}", out.stderr);
        assert!(out.stderr.contains("command : bad thing"), "{}", out.stderr);
        assert!(out.stderr.contains("command:1"), "{}", out.stderr);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_syntax_errors_point_at_the_callers_code() {
        let out = run(&spec("Write-Output 'ok'\nWrite-Output (1 +", None), None, Duration::from_secs(60)).await.unwrap();
        assert_eq!(out.exit_code, Some(1), "{out:?}");
        assert!(out.stdout.is_empty(), "nothing runs: {out:?}");
        assert!(out.stderr.contains("command:2"), "{}", out.stderr);
        assert!(!out.stderr.contains("LASTEXITCODE"), "the appended check must not show: {}", out.stderr);
    }

    /// pwsh 7 colours errors with ANSI escapes unless told otherwise. Skipped where pwsh is missing.
    #[cfg(windows)]
    #[tokio::test]
    async fn pwsh_errors_are_plain_text_and_exit_codes_survive() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        if !std::env::split_paths(&path).any(|dir| dir.join("pwsh.exe").is_file()) {
            eprintln!("pwsh is not installed; skipping");
            return;
        }
        let mut env = EnvMap::new();
        env.insert("TERM".into(), Some("xterm-test".into()));
        let cmd = "Write-Output \"héllo $env:TERM\"; Write-Error 'bad'; cmd /c exit 4";
        let spec = CommandSpec { env: Some(&env), ..spec(cmd, Some("pwsh")) };
        let out = run(&spec, None, Duration::from_secs(60)).await.unwrap();
        assert_eq!(out.exit_code, Some(4), "{out:?}");
        assert_eq!(out.stdout.trim_end(), "héllo xterm-test", "the script sees the caller's TERM: {out:?}");
        assert!(out.stderr.contains("bad") && !out.stderr.contains('\x1b'), "{:?}", out.stderr);
    }

    /// PowerShell starts with TERM=dumb, but neither the script nor what it runs see that.
    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_scripts_see_the_callers_term() {
        let cmd = "\"[$env:TERM]\"; cmd /c 'if defined TERM (echo child=%TERM%) else (echo child=none)'";
        for (term, expected) in [(None, ["[]", "child=none"]), (Some("vt100"), ["[vt100]", "child=vt100"])] {
            let mut env = EnvMap::new();
            env.insert("TERM".into(), term.map(str::to_string));
            let spec = CommandSpec { env: Some(&env), ..spec(cmd, None) };
            let out = run(&spec, None, Duration::from_secs(60)).await.unwrap();
            assert_eq!(out.stdout.lines().map(str::trim_end).collect::<Vec<_>>(), expected, "{out:?}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_keeps_quotes_and_operators() {
        let out = run(&spec(r#"echo "a & b" | findstr /c:"a & b" && echo 世界 100%"#, Some("cmd")), None, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        let lines: Vec<&str> = out.stdout.lines().map(str::trim_end).collect();
        assert_eq!(lines, [r#""a & b""#, "世界 100%"], "{out:?}");
    }

    #[tokio::test]
    async fn env_set_and_remove_and_stdin() {
        let mut env = EnvMap::new();
        env.insert("AR_TEST_SET".into(), Some("välue".into()));
        env.insert("PATH_AR_TEST_UNSET".into(), None);
        let cmd = if cfg!(windows) {
            "\"[$env:AR_TEST_SET][$env:PATH_AR_TEST_UNSET]\"; $t = [Console]::In.ReadToEnd(); \"in=$($t.Trim())\""
        } else {
            "echo \"[$AR_TEST_SET][$PATH_AR_TEST_UNSET]\"; read t; echo \"in=$t\""
        };
        let spec = CommandSpec { env: Some(&env), ..spec(cmd, None) };
        let out = run(&spec, Some("stdin 世界\n"), Duration::from_secs(60)).await.unwrap();
        assert_eq!(out.stdout.lines().collect::<Vec<_>>(), ["[välue][]", "in=stdin 世界"], "{out:?}");
    }

    /// Whether a process with this pid is still alive.
    fn alive(pid: u32) -> bool {
        let mut sys = sysinfo::System::new();
        let pid = sysinfo::Pid::from_u32(pid);
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        sys.process(pid).is_some()
    }

    /// A command that records its own pid and a sleeping child's pid, then sleeps.
    fn tree_command(pid_file: &std::path::Path) -> String {
        let f = pid_file.display().to_string();
        if cfg!(windows) {
            format!(
                "$c = Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','120','127.0.0.1'; \
                 Set-Content -LiteralPath '{f}' -Value \"$PID $($c.Id)\"; Start-Sleep 120"
            )
        } else {
            format!("sleep 120 & echo \"$$ $!\" > '{f}'; sleep 120")
        }
    }

    async fn read_pids(pid_file: &std::path::Path) -> Vec<u32> {
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(pid_file) {
                let pids: Vec<u32> = s.split_whitespace().filter_map(|p| p.parse().ok()).collect();
                if pids.len() == 2 {
                    return pids;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the test command did not write its pids");
    }

    async fn assert_all_gone(pids: &[u32]) {
        for _ in 0..50 {
            if pids.iter().all(|p| !alive(*p)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("processes still alive: {:?}", pids.iter().filter(|p| alive(**p)).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_tree() {
        let dir = std::env::temp_dir().join(format!("ar-shell-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("timeout.pids");
        let cmd = tree_command(&pid_file);
        // Generous: PowerShell can take seconds to start while other tests load the machine.
        let out = run(&spec(&cmd, None), None, Duration::from_secs(12)).await.unwrap();
        assert!(out.timed_out, "{out:?}");
        assert_eq!(out.exit_code, None);
        let pids = read_pids(&pid_file).await;
        assert_all_gone(&pids).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The call ends with the command's own process (not when everything it started has ended),
    /// and what it left running is killed.
    #[tokio::test]
    async fn main_exit_ends_the_call_and_kills_leftovers() {
        let dir = std::env::temp_dir().join(format!("ar-shell-leftover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("leftover.pids");
        let f = pid_file.display().to_string();
        let cmd = if cfg!(windows) {
            format!(
                "$c = Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','120','127.0.0.1'; \
                 Set-Content -LiteralPath '{f}' -Value \"$PID $($c.Id)\"; 'main-done'"
            )
        } else {
            format!("sleep 120 & echo \"$$ $!\" > '{f}'; echo main-done")
        };
        let out = run(&spec(&cmd, None), None, Duration::from_secs(90)).await.unwrap();
        assert!(!out.timed_out, "{out:?}");
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        assert!(out.stdout.contains("main-done"), "{out:?}");
        assert!(out.elapsed_ms < 45_000, "the call waited for the leftover: {out:?}");
        let pids = read_pids(&pid_file).await;
        assert_all_gone(&pids).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dropping_the_call_kills_the_whole_tree() {
        let dir = std::env::temp_dir().join(format!("ar-shell-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("cancel.pids");
        let cmd = tree_command(&pid_file);
        let spec = spec(&cmd, None);
        // What the gate does when a session ends: the call's future is dropped mid-run.
        let pids = tokio::select! {
            _ = run(&spec, None, Duration::from_secs(300)) => panic!("the command should still be running"),
            pids = read_pids(&pid_file) => pids,
        };
        assert_all_gone(&pids).await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
