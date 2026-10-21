//! An agent process: started in its own process tree (a Job Object on Windows, a new session and
//! process group elsewhere) so stopping it takes down whatever it started, with its standard input
//! written by one task and its output read line by line by another.
//!
//! - The prompt never travels in the command line: argument lists have length limits, and on
//!   Windows a `.cmd` shim re-parses them through cmd.exe. Everything the user types goes through
//!   standard input.
//! - Output lines are capped ([`MAX_LINE`]); a longer one ends the session rather than being cut
//!   into something that parses as a different message.
//! - Dropping the process kills its tree. On Windows the Job Object also kills it when the host
//!   process dies; on Unix a host that died leaves it running, so a restarted host stops what its
//!   predecessor left (see [`super::ChatManager::new`]) using the pid and start time kept on disk.

use std::{
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

/// Longest line read from an agent.
pub const MAX_LINE: usize = 32 * 1024 * 1024;
/// Bytes of standard error kept for error messages.
const STDERR_TAIL: usize = 4096;

/// Variables an agent must not inherit: a Claude Code that sees the variables of the Claude Code
/// that started the host (a developer running it from a session) thinks it runs nested.
const SCRUBBED_PREFIXES: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_EXECPATH",
    "AI_REMOTE_PS_",
    "AI_REMOTE_CMD_",
    "AI_REMOTE_CHAT_RUNNER",
];

/// Removes variables an agent must not inherit from `cmd`'s environment.
pub fn scrub_env(cmd: &mut tokio::process::Command) {
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if SCRUBBED_PREFIXES.iter().any(|p| k.starts_with(p)) {
            cmd.env_remove(&*k);
        }
    }
}

pub struct AgentProc {
    child: Box<dyn ChildWrapper>,
    pub pid: u32,
    /// Start time (seconds since the epoch) as the process table reports it, to recognise the
    /// process after a host restart.
    pub start: u64,
    stdin: Option<mpsc::Sender<String>>,
    stderr: Arc<Mutex<String>>,
}

/// A line of output, or why reading stopped.
pub type Line = Result<String, String>;

impl AgentProc {
    /// Starts `program args` in `cwd` with `env` changes. Returns the process and its output lines;
    /// the channel closes when the output ends.
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<(Self, mpsc::Receiver<Line>)> {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args).current_dir(cwd).envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        scrub_env(&mut cmd);
        let mut wrap = CommandWrap::from(cmd);
        #[cfg(windows)]
        wrap.wrap(process_wrap::tokio::CreationFlags(::windows::Win32::System::Threading::CREATE_NO_WINDOW));
        wrap.wrap(KillOnDrop);
        #[cfg(windows)]
        wrap.wrap(process_wrap::tokio::JobObject);
        #[cfg(unix)]
        wrap.wrap(process_wrap::tokio::ProcessSession);
        let mut child = wrap.spawn().with_context(|| format!("cannot start {}", program.display()))?;
        let pid = child.id().ok_or_else(|| anyhow!("{} exited at once", program.display()))?;
        let start = crate::tools::jobs::process_start_time(pid).unwrap_or(0);
        let stdin = child.stdin().take().ok_or_else(|| anyhow!("no standard input"))?;
        let stdout = child.stdout().take().ok_or_else(|| anyhow!("no standard output"))?;
        let stderr = child.stderr().take().ok_or_else(|| anyhow!("no standard error"))?;

        let (in_tx, mut in_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = in_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
            // Dropping stdin closes it: the agent sees the end of its input.
        });

        let (out_tx, out_rx) = mpsc::channel::<Line>(256);
        tokio::spawn(async move {
            let mut reader = BufReader::with_capacity(64 * 1024, stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).await {
                    Ok(0) => break,
                    Ok(_) if buf.len() > MAX_LINE => {
                        let _ = out_tx.send(Err(format!("the agent wrote a line over {} MiB", MAX_LINE >> 20))).await;
                        break;
                    }
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                        if line.is_empty() {
                            continue;
                        }
                        if out_tx.send(Ok(line)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = out_tx.send(Err(format!("reading the agent's output failed: {e}"))).await;
                        break;
                    }
                }
            }
        });

        let tail = Arc::new(Mutex::new(String::new()));
        let keep = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut t = keep.lock().unwrap();
                t.push_str(&line);
                t.push('\n');
                if t.len() > STDERR_TAIL * 2 {
                    let cut = t.len() - STDERR_TAIL;
                    let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(t.len());
                    t.replace_range(..cut, "");
                }
            }
        });

        Ok((Self { child, pid, start, stdin: Some(in_tx), stderr: tail }, out_rx))
    }

    /// Writes one line (a newline is added) to the agent's standard input.
    pub async fn write_line(&self, mut line: String) -> Result<()> {
        line.push('\n');
        let tx = self.stdin.as_ref().ok_or_else(|| anyhow!("the agent's input is closed"))?;
        tx.send(line).await.map_err(|_| anyhow!("the agent stopped reading its input"))
    }

    /// The end of what the agent wrote to standard error.
    pub fn stderr_tail(&self) -> String {
        let t = self.stderr.lock().unwrap();
        let s = t.trim();
        let start = s.len().saturating_sub(STDERR_TAIL);
        let start = (start..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(s.len());
        s[start..].to_string()
    }

    /// Whether the process has exited.
    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Ends the agent: closes its input, gives it `grace` to exit, then kills its whole tree.
    pub async fn shutdown(mut self, grace: Duration) {
        self.stdin = None;
        if tokio::time::timeout(grace, self.child.wait()).await.is_err() {
            let _ = self.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        }
        // The main process may be gone while processes it started still run: the tree goes too.
        let _ = self.child.start_kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_round_trip_and_shutdown_kills_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let script = "sleep 600 & echo child $!; while read l; do echo \"got $l\"; done";
        let (p, mut lines) =
            AgentProc::spawn(Path::new("/bin/sh"), &["-c".into(), script.into()], dir.path(), &Default::default())
                .unwrap();
        let first = lines.recv().await.unwrap().unwrap();
        let child: u32 = first.strip_prefix("child ").unwrap().parse().unwrap();
        p.write_line("hello".into()).await.unwrap();
        assert_eq!(lines.recv().await.unwrap().unwrap(), "got hello");
        p.shutdown(Duration::from_millis(500)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let alive =
            sys.process(sysinfo::Pid::from_u32(child)).is_some_and(|p| p.status() != sysinfo::ProcessStatus::Zombie);
        assert!(!alive, "the agent's child process must be stopped with it");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// The agent's input reaches it, and stopping it takes down what it started (a Job Object).
    #[tokio::test]
    async fn lines_round_trip_and_shutdown_kills_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let script = "$c = Start-Process -PassThru -WindowStyle Hidden ping -ArgumentList '-n','600','127.0.0.1'; \
                      Write-Output \"child $($c.Id)\"; while ($l = [Console]::In.ReadLine()) { Write-Output \"got $l\" }";
        let (p, mut lines) = AgentProc::spawn(
            Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            &["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script.into()],
            dir.path(),
            &Default::default(),
        )
        .unwrap();
        let first = loop {
            let l = lines.recv().await.unwrap().unwrap();
            if l.starts_with("child ") {
                break l;
            }
        };
        let child: u32 = first.strip_prefix("child ").unwrap().trim().parse().unwrap();
        p.write_line("hello".into()).await.unwrap();
        let got = loop {
            let l = lines.recv().await.unwrap().unwrap();
            if l.starts_with("got ") {
                break l;
            }
        };
        assert_eq!(got, "got hello");
        p.shutdown(Duration::from_millis(500)).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        assert!(
            sys.process(sysinfo::Pid::from_u32(child)).is_none(),
            "the agent's child process must be stopped with it"
        );
    }
}
