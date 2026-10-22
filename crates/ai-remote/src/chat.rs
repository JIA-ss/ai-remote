//! `ai-remote chat`: coding agents on a paired host (Claude Code, Codex, ACP agents) from a
//! terminal or a script. `list` shows what is installed there; `start` runs one in a folder and
//! talks to it (each line typed is a message; Ctrl+C stops the running turn, a second one leaves);
//! `run` hands it one task and prints the answer; `attach`, `send`, `interrupt` and `end` work on
//! sessions already running. The host's levels, approval and audit apply as to any tool call, and
//! the agent runs on the host without asking before it acts.
//!
//! Exit codes as for the other host commands: 0 done, 2 refused or the turn failed, 1 the call could
//! not be made.

use std::io::{IsTerminal, Write as _};

use anyhow::{Result, anyhow};
use ar_core::Profile;
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::remote::{Target, call, exit, print_json, refused_transfer};

#[derive(Subcommand, Debug)]
pub enum ChatCommand {
    /// The coding agents installed on the host: version, sign-in, where.
    List {
        /// Look again instead of using the answer of the last minute.
        #[arg(long)]
        refresh: bool,
        #[command(flatten)]
        target: Target,
    },
    /// The sessions on the host.
    Sessions {
        #[command(flatten)]
        target: Target,
    },
    /// Start an agent in a folder on the host and talk to it: each line is a message.
    Start(StartArgs),
    /// Give an agent one task, print its answer and end the session.
    Run(RunArgs),
    /// Watch a running session (and, if you started it, keep talking).
    Attach {
        session: String,
        /// Show events after this one (0: the whole conversation).
        #[arg(long, default_value_t = 0)]
        since: u64,
        #[command(flatten)]
        target: Target,
    },
    /// Send one message to a session.
    Send {
        session: String,
        text: String,
        /// Wait for the turn to end and print the answer.
        #[arg(long)]
        wait: bool,
        /// Add the message to the running turn (agents that can take it, see `chat list`);
        /// otherwise, or with no turn running, it is queued as usual.
        #[arg(long)]
        steer: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Stop a session's running turn.
    Interrupt {
        session: String,
        #[command(flatten)]
        target: Target,
    },
    /// End a session (its agent process stops).
    End {
        session: String,
        #[command(flatten)]
        target: Target,
    },
    /// [host] Allow devices with level admin to start agents here (the default).
    Enable,
    /// [host] Forbid starting agents here, even for bypassed devices; ends every session.
    Disable,
    /// [host] Whether agents may start here, and the sessions.
    Status,
}

#[derive(Args, Debug)]
pub struct StartArgs {
    /// The agent (`ai-remote chat list` shows the ids: claude, codex, gemini, ...).
    pub runtime: String,
    /// The folder on the host the agent works in.
    #[arg(long)]
    pub cwd: String,
    /// A first message.
    #[arg(long)]
    pub prompt: Option<String>,
    /// A model instead of the agent's default.
    #[arg(long)]
    pub model: Option<String>,
    /// Ignore the folder's own agent settings, hooks and MCP servers where the agent supports it.
    #[arg(long)]
    pub hardened: bool,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    pub runtime: String,
    /// The task.
    pub task: String,
    #[arg(long)]
    pub cwd: String,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub hardened: bool,
    /// Keep the session instead of ending it after the answer.
    #[arg(long)]
    pub keep: bool,
    #[command(flatten)]
    pub target: Target,
}

pub async fn command(profile: &Profile, cmd: ChatCommand, json_out: bool) -> Result<()> {
    match cmd {
        ChatCommand::List { refresh, target } => {
            let v = call(profile, &target, "chat_runtimes", json!({"refresh": refresh})).await?.ok();
            if json_out {
                print_json(&v);
            } else {
                print_runtimes(&v);
            }
        }
        ChatCommand::Sessions { target } => {
            let v = call(profile, &target, "chat_sessions", json!({})).await?.ok();
            if json_out {
                print_json(&v);
            } else {
                for s in v["sessions"].as_array().into_iter().flatten() {
                    println!(
                        "{}  {:<12} {:<12} {:>3} turns  {}  (by {})",
                        s["id"].as_str().unwrap_or("?"),
                        s["runtime"].as_str().unwrap_or("?"),
                        s["state"].as_str().unwrap_or("?"),
                        s["turns"].as_u64().unwrap_or(0),
                        s["cwd"].as_str().unwrap_or("?"),
                        s["by"]["device"].as_str().unwrap_or("?"),
                    );
                }
            }
        }
        ChatCommand::Start(a) => {
            let mut args = json!({"runtime": a.runtime, "cwd": a.cwd, "hardened": a.hardened});
            if let Some(p) = a.prompt {
                args["prompt"] = p.into();
            }
            if let Some(m) = a.model {
                args["model"] = m.into();
            }
            let started = call(profile, &a.target, "chat_start", args).await?.ok();
            let mut chat = Conversation::open(profile, &a.target, &started, 0).await?;
            if !json_out {
                eprintln!(
                    "{} on {} in {} — type a message and Enter; Ctrl+C stops a turn, twice leaves (the session goes on; `ai-remote chat end {}` ends it)",
                    chat.hello["runtime"]["name"].as_str().unwrap_or("agent"),
                    chat.hello["host"].as_str().unwrap_or("?"),
                    chat.hello["cwd"].as_str().unwrap_or("?"),
                    chat.session()
                );
            }
            chat.interactive(json_out).await?;
        }
        ChatCommand::Attach { session, since, target } => {
            let issued = call(profile, &target, "chat_attach", json!({"session": session})).await?.ok();
            let mut chat = Conversation::open(profile, &target, &issued, since).await?;
            chat.interactive(json_out).await?;
        }
        ChatCommand::Run(a) => {
            let mut args = json!({"runtime": a.runtime, "cwd": a.cwd, "hardened": a.hardened, "prompt": a.task});
            if let Some(m) = a.model {
                args["model"] = m.into();
            }
            let started = call(profile, &a.target, "chat_start", args).await?.ok();
            let mut chat = Conversation::open(profile, &a.target, &started, 0).await?;
            let end = chat.until_turn_end(json_out).await?;
            if !a.keep {
                chat.send(json!({"end": {}})).await;
            }
            chat.send(json!({"bye": {}})).await;
            finish_turn(&end, json_out);
        }
        ChatCommand::Send { session, text, wait, steer, target } => {
            let issued = call(profile, &target, "chat_attach", json!({"session": session})).await?.ok();
            let since = issued["seq"].as_u64().unwrap_or(0);
            let mut chat = Conversation::open(profile, &target, &issued, since).await?;
            let mode = if steer { "steer" } else { "queue" };
            chat.send(json!({"input": {"text": text, "mode": mode}})).await;
            if wait {
                let end = chat.until_turn_end(json_out).await?;
                chat.send(json!({"bye": {}})).await;
                finish_turn(&end, json_out);
            } else {
                chat.send(json!({"bye": {}})).await;
            }
        }
        ChatCommand::Interrupt { session, target } => {
            let v = call(profile, &target, "chat_interrupt", json!({"session": session})).await?.ok();
            if json_out { print_json(&v) } else { println!("stopping the running turn of {session}") }
        }
        ChatCommand::End { session, target } => {
            let v = call(profile, &target, "chat_end", json!({"session": session})).await?.ok();
            if json_out { print_json(&v) } else { println!("ending {session}") }
        }
        ChatCommand::Enable | ChatCommand::Disable | ChatCommand::Status => {
            unreachable!("handled by the host commands")
        }
    }
    Ok(())
}

fn print_runtimes(v: &Value) {
    if v["host"]["can_start"] == false {
        println!("agents cannot start on this host: {}", v["host"]["reason"].as_str().unwrap_or("?"));
        return;
    }
    if let Some(e) = v["host"]["config_error"].as_str() {
        println!("agents are off on this host because its chat.toml cannot be read: {e}");
    } else if v["host"]["enabled"] == false {
        println!("agents are switched off on this host (someone there can run: ai-remote chat enable)");
    }
    for r in v["runtimes"].as_array().into_iter().flatten() {
        let verdict = r["verdict"].as_str().unwrap_or("?");
        if verdict == "missing" {
            continue;
        }
        let login = match r["login"]["state"].as_str() {
            Some("logged_in") => {
                format!("signed in{}", r["login"]["method"].as_str().map(|m| format!(" ({m})")).unwrap_or_default())
            }
            Some("logged_out") => "NOT signed in".into(),
            _ => "sign-in unknown".into(),
        };
        println!(
            "{:<12} {:<20} {:<10} {:<14} {:<22} {}",
            r["id"].as_str().unwrap_or("?"),
            r["name"].as_str().unwrap_or("?"),
            r["version"].as_str().unwrap_or("-"),
            verdict,
            login,
            r["path"].as_str().unwrap_or(""),
        );
        if let Some(d) = r["detail"].as_str() {
            println!("{:12} {d}", "");
        }
    }
    let missing: Vec<&str> = v["runtimes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["verdict"] == "missing")
        .filter_map(|r| r["id"].as_str())
        .collect();
    if !missing.is_empty() {
        println!("not installed: {}", missing.join(", "));
    }
}

fn finish_turn(end: &Value, json_out: bool) {
    if json_out {
        return;
    }
    match end["status"].as_str() {
        Some("completed") => exit(0),
        Some(status) => {
            eprintln!("turn {status}{}", end["reason"].as_str().map(|r| format!(": {r}")).unwrap_or_default());
            exit(2)
        }
        None => exit(2),
    }
}

/// An open chat channel through the controller agent.
struct Conversation {
    hello: Value,
    reader: BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
    writer: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    printer: Printer,
}

impl Conversation {
    async fn open(profile: &Profile, target: &Target, issued: &Value, since: u64) -> Result<Self> {
        let ticket = issued["ticket"].as_str().ok_or_else(|| anyhow!("the host issued no chat ticket"))?;
        let mut conn = ar_controller::connect_agent(profile).await?;
        let hello = conn
            .request(&json!({"op": "chat", "host": target.host, "request": {"ticket": ticket, "since": since}}))
            .await
            .map_err(refused_transfer)?;
        let (stream, buffered) = conn.into_raw();
        let (read, write) = tokio::io::split(stream);
        let read: Box<dyn tokio::io::AsyncRead + Unpin + Send> = Box::new(std::io::Cursor::new(buffered).chain(read));
        Ok(Self { hello, reader: BufReader::new(read), writer: Box::new(write), printer: Printer::default() })
    }

    fn session(&self) -> &str {
        self.hello["session"].as_str().unwrap_or("?")
    }

    async fn send(&mut self, v: Value) {
        let line = format!("{v}\n");
        let _ = self.writer.write_all(line.as_bytes()).await;
        let _ = self.writer.flush().await;
    }

    /// The next event line, or `None` when the channel ended.
    async fn next(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        Ok(serde_json::from_str(&line).ok())
    }

    async fn until_turn_end(&mut self, json_out: bool) -> Result<Value> {
        loop {
            let Some(ev) = self.next().await? else { return Err(anyhow!("the host ended the chat")) };
            self.printer.show(&ev, json_out);
            if ev["t"] == "turn_end" {
                return Ok(ev);
            }
        }
    }

    async fn interactive(&mut self, json_out: bool) -> Result<()> {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        let mut stdin_open = true;
        let mut ctrl_c = 0u8;
        let running = |s: &str| matches!(s, "running" | "starting" | "interrupting");
        let mut state = self.hello["state"].as_str().unwrap_or("").to_string();
        // Events up to this one are the replay; the live state is known only after them.
        let caught_up_at = self.hello["seq"].as_u64().unwrap_or(0);
        let mut seq = 0u64;
        // Standard input ended (a script): leave once the agent is idle.
        let mut leave_when_idle = false;
        loop {
            if leave_when_idle && seq >= caught_up_at && !running(&state) {
                self.send(json!({"bye": {}})).await;
                leave_when_idle = false;
            }
            tokio::select! {
                ev = self.next() => {
                    let Some(ev) = ev? else { break };
                    seq = ev["seq"].as_u64().unwrap_or(seq);
                    if ev["t"] == "state" {
                        state = ev["state"].as_str().unwrap_or("").to_string();
                        if state == "ended" {
                            self.printer.show(&ev, json_out);
                            break;
                        }
                    }
                    if ev["t"] == "bye" {
                        break;
                    }
                    self.printer.show(&ev, json_out);
                }
                line = stdin.next_line(), if stdin_open => match line? {
                    Some(text) if !text.trim().is_empty() => {
                        ctrl_c = 0;
                        self.send(json!({"input": {"text": text}})).await;
                    }
                    Some(_) => {}
                    None => {
                        stdin_open = false;
                        leave_when_idle = !std::io::stdin().is_terminal();
                    }
                },
                _ = tokio::signal::ctrl_c() => {
                    ctrl_c += 1;
                    if ctrl_c == 1 && running(&state) {
                        eprintln!("\n(stopping the turn; Ctrl+C again to leave)");
                        self.send(json!({"interrupt": {}})).await;
                    } else {
                        self.send(json!({"bye": {}})).await;
                        break;
                    }
                }
            }
        }
        if !json_out {
            eprintln!("(left {}; it goes on until ended: ai-remote chat end {})", self.session(), self.session());
        }
        Ok(())
    }
}

/// Prints events as a conversation.
#[derive(Default)]
struct Printer {
    /// Inside streamed text (no newline printed yet).
    open: bool,
    thinking: bool,
}

impl Printer {
    fn show(&mut self, ev: &Value, json_out: bool) {
        if json_out {
            println!("{ev}");
            return;
        }
        let mut out = std::io::stdout();
        let t = ev["t"].as_str().unwrap_or("");
        let close = |p: &mut Printer, out: &mut std::io::Stdout| {
            if p.open {
                let _ = out.write_all(b"\n");
                p.open = false;
            }
        };
        match t {
            "user" => {
                close(self, &mut out);
                let steer = if ev["steer"] == true { "(steer) " } else { "" };
                println!("\n> {steer}{}", ev["text"].as_str().unwrap_or(""));
            }
            "message" => {
                if self.thinking {
                    close(self, &mut out);
                    self.thinking = false;
                }
                let text = ev["text"].as_str().unwrap_or("");
                if ev["delta"] == true {
                    let _ = out.write_all(text.as_bytes());
                    self.open = !text.ends_with('\n');
                } else {
                    close(self, &mut out);
                    println!("{text}");
                }
            }
            "thought" if !self.thinking => {
                close(self, &mut out);
                print!("(thinking) ");
                self.thinking = true;
                self.open = true;
            }
            "tool_call" => {
                close(self, &mut out);
                self.thinking = false;
                println!("▸ {}", ev["title"].as_str().unwrap_or("tool"));
            }
            "tool_update" if ev["status"] == "failed" => {
                close(self, &mut out);
                let text = ev["content"][0]["text"].as_str().unwrap_or("").lines().next().unwrap_or("");
                println!("  ✗ failed {text}");
            }
            "plan" => {
                close(self, &mut out);
                for e in ev["entries"].as_array().into_iter().flatten() {
                    let mark = match e["status"].as_str() {
                        Some("completed") => "☑",
                        Some("in_progress") => "▶",
                        _ => "☐",
                    };
                    println!("  {mark} {}", e["content"].as_str().unwrap_or(""));
                }
            }
            "question" => {
                close(self, &mut out);
                println!("? {}", ev["prompt"].as_str().unwrap_or(""));
            }
            "turn_end" => {
                close(self, &mut out);
                self.thinking = false;
                let secs = ev["duration_ms"].as_u64().unwrap_or(0) as f64 / 1000.0;
                match ev["status"].as_str() {
                    Some("completed") => eprintln!("— done in {secs:.1} s"),
                    Some(s) => eprintln!(
                        "— {s} after {secs:.1} s{}",
                        ev["reason"].as_str().map(|r| format!(": {r}")).unwrap_or_default()
                    ),
                    None => {}
                }
            }
            "notice" => {
                close(self, &mut out);
                eprintln!("! {}", ev["text"].as_str().unwrap_or(""));
            }
            "error" => {
                close(self, &mut out);
                eprintln!("! {}", ev["text"].as_str().unwrap_or(""));
            }
            _ => {}
        }
        let _ = out.flush();
    }
}
