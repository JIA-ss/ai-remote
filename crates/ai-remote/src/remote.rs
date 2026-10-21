//! Operating a paired host from a terminal or a script, one command per kind of work: `exec`,
//! `job`, `shot`, `windows`, `info`, `doctor --host` and the file commands (`ls`, `stat`, `mkdir`,
//! `rm`, `push`, `pull`). Each one is a tool call through the controller agent (push and pull add a
//! transfer on the host's files channel), so the host's levels, policy, approval and audit apply
//! exactly as they do to an agent's calls; `ai-remote call` remains for any other tool.
//!
//! The host is `--host`, else `AI_REMOTE_HOST`, else this device's default host (`ai-remote hosts
//! default`), else the only paired host.
//!
//! Exit codes: 0 done; 2 the host refused the call or the tool failed; 1 the call could not be made
//! (no agent, host unreachable, bad arguments). `exec` exits with the remote command's own code
//! instead (124 when it timed out, 255 when it did not run), like ssh; `job wait` with the job's.

use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Profile;
use base64::Engine;
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};

/// Which host a command runs on.
#[derive(Args, Debug, Clone, Default)]
pub struct Target {
    /// Host to run on (its name or device id). Default: AI_REMOTE_HOST, then this device's default host
    /// (`ai-remote hosts default`), then the only paired host.
    #[arg(long, env = "AI_REMOTE_HOST")]
    pub host: Option<String>,
}

#[derive(Args, Debug)]
pub struct ExecArgs {
    /// The command line, quoted as one argument: PowerShell on Windows hosts, sh elsewhere.
    pub command: String,
    /// powershell (Windows default), pwsh or cmd; sh (default elsewhere), bash or zsh.
    #[arg(long)]
    pub shell: Option<String>,
    /// Working directory on the host.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Seconds before the command is killed (1-600, default 60); use `job start` for longer work.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,
    /// Environment variable for the command (repeat for several); the owner sees only names.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Send this file to the command's standard input (`-`: this terminal's standard input).
    #[arg(long, value_name = "FILE")]
    pub stdin: Option<PathBuf>,
    /// Why; shown to the host owner in approval prompts and the audit log.
    #[arg(long)]
    pub intent: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Subcommand, Debug)]
pub enum JobCommand {
    /// Start a command in the background on the host and print its job id.
    Start {
        /// The command line, quoted as one argument.
        command: String,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        /// Environment variable for the job (repeat for several); not saved with the job.
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        #[arg(long)]
        intent: Option<String>,
        #[command(flatten)]
        target: Target,
    },
    /// List the host's background jobs.
    List {
        #[command(flatten)]
        target: Target,
    },
    /// One job's status and the end of its log.
    Status {
        id: String,
        /// Bytes of the log's end to show (0-65536).
        #[arg(long, default_value_t = 4096)]
        tail: u64,
        #[command(flatten)]
        target: Target,
    },
    /// Print a job's log; --follow keeps printing until the job has ended.
    Log {
        id: String,
        /// Byte offset to start at; negative counts from the end (-4096: the last 4 KiB).
        #[arg(long, default_value_t = 0, allow_negative_numbers = true)]
        offset: i64,
        #[arg(short, long)]
        follow: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Wait for a job to end and exit with its exit code (124: still running at --timeout; 137: killed;
    /// 255: unknown).
    Wait {
        id: String,
        /// Give up after this many seconds (default: wait as long as it runs).
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        #[command(flatten)]
        target: Target,
    },
    /// Stop a job and every process it started.
    Kill {
        id: String,
        #[arg(long)]
        intent: Option<String>,
        #[command(flatten)]
        target: Target,
    },
}

#[derive(Args, Debug)]
pub struct ShotArgs {
    /// Image file to write (default: screenshot-<time>.jpg here); a .png name asks for PNG.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Monitor index (see `ai-remote info`); default: the primary monitor.
    #[arg(long)]
    pub monitor: Option<usize>,
    /// Longest edge of the image in pixels (320-2576); the host never upscales.
    #[arg(long, default_value_t = 2576)]
    pub max_long_edge: u32,
    /// jpeg or png (default: from the file name, else jpeg).
    #[arg(long)]
    pub format: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct WindowsArgs {
    /// Only windows whose title or program name contains this.
    #[arg(long)]
    pub filter: Option<String>,
    /// Also untitled windows and click-through overlays.
    #[arg(long)]
    pub all: bool,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct PsArgs {
    /// Only processes whose name (or executable file name) contains this.
    pub name: Option<String>,
    /// Only this process.
    #[arg(long, conflicts_with = "name")]
    pub pid: Option<u32>,
    /// At most this many (default 300, maximum 2000).
    #[arg(long)]
    pub limit: Option<usize>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct KillArgs {
    /// The process to stop, by pid.
    #[arg(long, required_unless_present = "name", conflicts_with = "name")]
    pub pid: Option<u32>,
    /// Every process with exactly this name (case-insensitive), e.g. game.exe.
    #[arg(long)]
    pub name: Option<String>,
    /// Only that process, not the processes it started.
    #[arg(long)]
    pub no_tree: bool,
    /// Why (shown to the owner and in the audit log).
    #[arg(long)]
    pub intent: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct PathArgs {
    /// Absolute path on the host.
    pub path: String,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    /// Absolute path of a folder on the host.
    pub path: String,
    /// Only names matching this pattern (* and ? wildcards), e.g. "*.log".
    #[arg(long)]
    pub pattern: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct MkdirArgs {
    /// Absolute path of the folder (missing parents are created too).
    pub path: String,
    #[arg(long)]
    pub intent: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct RmArgs {
    /// Absolute path of a file, link or folder on the host.
    pub path: String,
    /// Delete a folder with everything in it.
    #[arg(short, long)]
    pub recursive: bool,
    #[arg(long)]
    pub intent: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct PushArgs {
    /// The file to send.
    pub local: PathBuf,
    /// Absolute path on the host: the file to write, or a folder to put it in (ends with a slash or
    /// already exists there).
    pub remote: String,
    /// Replace the file on the host when it exists.
    #[arg(short, long)]
    pub force: bool,
    /// Create missing folders on the host.
    #[arg(long)]
    pub mkdir: bool,
    #[arg(long)]
    pub intent: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

#[derive(Args, Debug)]
pub struct PullArgs {
    /// Absolute path of the file on the host.
    pub remote: String,
    /// Where to save it: a file, or a folder to put it in (default: here, under its own name).
    pub local: Option<PathBuf>,
    /// Replace the local file when it exists.
    #[arg(short, long)]
    pub force: bool,
    #[arg(long)]
    pub intent: Option<String>,
    #[command(flatten)]
    pub target: Target,
}

/// Exits after flushing what was printed (`std::process::exit` skips the buffers).
pub(crate) fn exit(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code)
}

pub(crate) fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// A tool result as the agent relays it (`content`, `isError`, `structuredContent`).
pub(crate) struct Reply(Value);

impl Reply {
    pub(crate) fn is_error(&self) -> bool {
        self.0["isError"].as_bool().unwrap_or(false)
    }

    pub(crate) fn texts(&self) -> Vec<&str> {
        let blocks = self.0["content"].as_array().map(Vec::as_slice).unwrap_or_default();
        blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect()
    }

    /// The JSON value: structured content, else the first text block parsed.
    pub(crate) fn value(&self) -> Value {
        if let Some(v) = self.0.get("structuredContent").filter(|v| !v.is_null()) {
            return v.clone();
        }
        self.texts().first().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null)
    }

    /// The text block after the JSON one (a job's log), if any.
    fn second_text(&self) -> Option<&str> {
        self.texts().get(1).copied()
    }

    fn image(&self) -> Option<(String, String)> {
        let blocks = self.0["content"].as_array()?;
        let b = blocks.iter().find(|b| b["type"] == "image")?;
        Some((b["mimeType"].as_str()?.to_string(), b["data"].as_str()?.to_string()))
    }

    /// The value, or the host's refusal on stderr and exit code 2.
    pub(crate) fn ok(self) -> Value {
        if self.is_error() {
            eprintln!("{}", in_cli_terms(&self.texts().join("\n")));
            exit(2);
        }
        self.value()
    }
}

/// A host message that names a tool argument, reworded with the option that sets it here.
fn in_cli_terms(text: &str) -> String {
    text.replace("pass overwrite: true", "pass --force")
        .replace("pass mkdir: true", "pass --mkdir")
        .replace("pass recursive: true", "pass -r")
}

pub(crate) async fn call(profile: &Profile, target: &Target, tool: &str, args: Value) -> Result<Reply> {
    let mut conn = ar_controller::connect_agent(profile).await?;
    let v =
        conn.request(&json!({"op": "call", "host": target.host, "tool": tool, "args": args, "person": false})).await?;
    Ok(Reply(v))
}

/// Sets `key` in `args` when `value` is present.
fn put(args: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
    if let Some(v) = value {
        args.insert(key.into(), v.into());
    }
}

fn env_map(pairs: &[String]) -> Result<Option<Value>> {
    if pairs.is_empty() {
        return Ok(None);
    }
    let mut map = Map::new();
    for pair in pairs {
        let (k, v) = pair.split_once('=').ok_or_else(|| anyhow!("--env takes KEY=VALUE, not '{pair}'"))?;
        if k.is_empty() {
            bail!("--env takes KEY=VALUE, not '{pair}'");
        }
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    Ok(Some(Value::Object(map)))
}

pub async fn exec(profile: &Profile, a: ExecArgs, json_out: bool) -> Result<()> {
    // Anything that keeps the command from running exits 255, so its own codes stay unambiguous.
    let reply = match exec_call(profile, &a).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {e:#}");
            exit(255);
        }
    };
    if reply.is_error() {
        eprintln!("{}", reply.texts().join("\n"));
        exit(255);
    }
    let v = reply.value();
    if json_out {
        print_json(&v);
    } else {
        print!("{}", v["stdout"].as_str().unwrap_or_default());
        eprint!("{}", v["stderr"].as_str().unwrap_or_default());
    }
    if v["timed_out"] == true {
        if !json_out {
            eprintln!("ai-remote: {}", v["note"].as_str().unwrap_or("the command timed out and was killed"));
        }
        exit(124);
    }
    exit(v["exit_code"].as_i64().map(|c| c as i32).unwrap_or(255))
}

async fn exec_call(profile: &Profile, a: &ExecArgs) -> Result<Reply> {
    let mut args = Map::new();
    args.insert("command".into(), a.command.clone().into());
    put(&mut args, "shell", a.shell.clone());
    put(&mut args, "cwd", a.cwd.clone());
    put(&mut args, "timeout_secs", a.timeout);
    put(&mut args, "env", env_map(&a.env)?);
    put(&mut args, "intent", a.intent.clone());
    if let Some(path) = &a.stdin {
        let text = if path.as_os_str() == "-" {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).context("read standard input")?;
            s
        } else {
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
        };
        args.insert("stdin".into(), text.into());
    }
    call(profile, &a.target, "shell", Value::Object(args)).await
}

fn local_time(ms: Option<u64>) -> String {
    ms.and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "-".into())
}

fn duration(ms: Option<u64>) -> String {
    let Some(ms) = ms else { return "-".into() };
    let s = ms / 1000;
    match s {
        0..60 => format!("{:.1} s", ms as f64 / 1000.0),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

fn job_state(v: &Value) -> String {
    match (v["state"].as_str().unwrap_or("?"), v["exit_code"].as_i64()) {
        ("exited", Some(code)) => format!("exited ({code})"),
        (state, _) => state.to_string(),
    }
}

pub async fn job(profile: &Profile, command: JobCommand, json_out: bool) -> Result<()> {
    match command {
        JobCommand::Start { command, shell, cwd, env, intent, target } => {
            let mut args = Map::new();
            args.insert("command".into(), command.into());
            put(&mut args, "shell", shell);
            put(&mut args, "cwd", cwd);
            put(&mut args, "env", env_map(&env)?);
            put(&mut args, "intent", intent);
            let v = call(profile, &target, "jobs_start", Value::Object(args)).await?.ok();
            if json_out {
                print_json(&v);
            } else {
                println!("{}", v["id"].as_str().unwrap_or("?"));
                eprintln!(
                    "started (pid {}); follow it with: ai-remote job log {} -f",
                    v["pid"],
                    v["id"].as_str().unwrap_or("?")
                );
            }
        }
        JobCommand::List { target } => {
            let v = call(profile, &target, "jobs_status", json!({})).await?.ok();
            if json_out {
                print_json(&v);
                return Ok(());
            }
            let jobs = v["jobs"].as_array().cloned().unwrap_or_default();
            if jobs.is_empty() {
                println!("No jobs on this host.");
            }
            for j in jobs {
                println!(
                    "{:<10} {:<12} {:<9} {:<19} {}",
                    j["id"].as_str().unwrap_or("?"),
                    job_state(&j),
                    duration(j["duration_ms"].as_u64()),
                    local_time(j["started_at"].as_u64()),
                    j["command"].as_str().unwrap_or("").replace('\n', " ")
                );
            }
        }
        JobCommand::Status { id, tail, target } => {
            let reply = call(profile, &target, "jobs_status", json!({"id": id, "tail_bytes": tail})).await?;
            let tail = reply.second_text().map(str::to_string);
            let mut v = reply.ok();
            if json_out {
                v["log_tail"] = json!(tail);
                print_json(&v);
                return Ok(());
            }
            print_job(&v);
            if let Some(t) = tail {
                println!("--- end of log ---");
                print!("{t}");
                if !t.ends_with('\n') {
                    println!();
                }
            }
        }
        JobCommand::Log { id, offset, follow, target } => {
            job_log(profile, &target, &id, offset, follow, json_out).await?
        }
        JobCommand::Wait { id, timeout, target } => {
            let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(s));
            loop {
                let left = deadline.map(|d| d.saturating_duration_since(Instant::now()).as_secs());
                let wait = left.unwrap_or(120).clamp(1, 120);
                let reply = call(profile, &target, "jobs_wait", json!({"id": id, "timeout_secs": wait})).await?;
                let v = reply.ok();
                let running = v["running"] == true;
                if running && left.is_none_or(|l| l > wait) {
                    continue;
                }
                if json_out {
                    print_json(&v);
                } else {
                    eprintln!("job {}: {}", v["id"].as_str().unwrap_or("?"), job_state(&v));
                }
                exit(match (running, v["state"].as_str(), v["exit_code"].as_i64()) {
                    (true, ..) => 124,
                    (_, Some("killed"), _) => 137,
                    (_, _, Some(code)) => code as i32,
                    _ => 255,
                });
            }
        }
        JobCommand::Kill { id, intent, target } => {
            let mut args = Map::new();
            args.insert("id".into(), id.into());
            put(&mut args, "intent", intent);
            let v = call(profile, &target, "jobs_kill", Value::Object(args)).await?.ok();
            if json_out {
                print_json(&v);
            } else {
                println!("job {}: {}", v["id"].as_str().unwrap_or("?"), job_state(&v));
            }
        }
    }
    Ok(())
}

fn print_job(v: &Value) {
    println!(
        "job {}: {}, pid {}, started {}, ran {}",
        v["id"].as_str().unwrap_or("?"),
        job_state(v),
        v["pid"],
        local_time(v["started_at"].as_u64()),
        duration(v["duration_ms"].as_u64())
    );
    println!("command: {}", v["command"].as_str().unwrap_or(""));
    if let Some(n) = v["leftover_processes"].as_u64().filter(|n| *n > 0) {
        println!("{n} process(es) it started still run");
    }
    if let Some(note) = v["note"].as_str() {
        println!("note: {note}");
    }
}

async fn job_log(
    profile: &Profile,
    target: &Target,
    id: &str,
    mut offset: i64,
    follow: bool,
    json_out: bool,
) -> Result<()> {
    loop {
        let reply =
            call(profile, target, "jobs_log", json!({"id": id, "offset": offset, "max_bytes": 1 << 20})).await?;
        let text = reply.second_text().unwrap_or_default().to_string();
        let header = reply.ok();
        if json_out {
            println!("{}", json!({"offset": header["offset"], "next_offset": header["next_offset"], "text": text}));
        } else {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        offset = header["next_offset"].as_i64().unwrap_or(offset);
        if header["eof"] != true {
            continue;
        }
        if !follow || header["running"] != true {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn shot(profile: &Profile, a: ShotArgs, json_out: bool) -> Result<()> {
    let png = match a.format.as_deref() {
        Some(f) if f.eq_ignore_ascii_case("png") => true,
        Some(f) if f.eq_ignore_ascii_case("jpeg") || f.eq_ignore_ascii_case("jpg") => false,
        Some(f) => bail!("--format is jpeg or png, not '{f}'"),
        None => a.out.as_ref().and_then(|o| o.extension()).is_some_and(|e| e.eq_ignore_ascii_case("png")),
    };
    let mut args = Map::new();
    args.insert("max_long_edge".into(), a.max_long_edge.into());
    args.insert("format".into(), if png { "png" } else { "jpeg" }.into());
    put(&mut args, "monitor", a.monitor);
    let reply = call(profile, &a.target, "screenshot", Value::Object(args)).await?;
    let image = reply.image();
    let mut meta = reply.ok();
    let (mime, data) = image.ok_or_else(|| anyhow!("the host sent no image"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).context("decode the image")?;
    // A PNG over the result budget comes back as JPEG (the host says so in `warning`): the file name
    // follows what arrived, so its extension never lies about its content.
    let ext = if mime == "image/png" { "png" } else { "jpg" };
    let out = match a.out {
        Some(out) if png && ext != "png" => out.with_extension(ext),
        Some(out) => out,
        None => PathBuf::from(format!("screenshot-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S"))),
    };
    std::fs::write(&out, &bytes).with_context(|| format!("write {}", out.display()))?;
    if json_out {
        meta["saved"] = json!(out.display().to_string());
        print_json(&meta);
        return Ok(());
    }
    let img = &meta["image"];
    println!(
        "saved {} ({}x{} of a {}x{} monitor, {} KiB)",
        out.display(),
        img["width"],
        img["height"],
        img["native_width"],
        img["native_height"],
        bytes.len() / 1024
    );
    if let Some(w) = meta["warning"].as_str() {
        eprintln!("warning: {w}");
    }
    Ok(())
}

pub async fn windows(profile: &Profile, a: WindowsArgs, json_out: bool) -> Result<()> {
    let mut args = Map::new();
    put(&mut args, "filter", a.filter);
    if a.all {
        args.insert("include_untitled".into(), true.into());
        args.insert("include_overlays".into(), true.into());
    }
    let v = call(profile, &a.target, "list_windows", Value::Object(args)).await?.ok();
    if json_out {
        print_json(&v);
        return Ok(());
    }
    for w in v["windows"].as_array().cloned().unwrap_or_default() {
        let mut marks = Vec::new();
        for (key, mark) in [("focused", "focused"), ("minimized", "minimized"), ("maximized", "maximized")] {
            if w[key] == true {
                marks.push(mark.to_string());
            }
        }
        for flag in w["flags"].as_array().cloned().unwrap_or_default() {
            marks.extend(flag.as_str().map(str::to_string));
        }
        let marks = if marks.is_empty() { String::new() } else { format!("  [{}]", marks.join(", ")) };
        println!(
            "{:<10} {:<7} {:<24} {}{marks}",
            w["window_id"],
            w["pid"],
            w["app"].as_str().unwrap_or("?"),
            w["title"].as_str().unwrap_or("")
        );
    }
    if let Some(warning) = v["warning"].as_str() {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

pub async fn info(profile: &Profile, target: Target, json_out: bool) -> Result<()> {
    let v = call(profile, &target, "system_info", json!({})).await?.ok();
    if json_out {
        print_json(&v);
        return Ok(());
    }
    let s = |v: &Value| v.as_str().unwrap_or("?").to_string();
    println!("host:     {} (ai-remote {})", s(&v["host"]), s(&v["ai_remote_version"]));
    let os = &v["os"];
    println!("os:       {} {} ({})", s(&os["name"]), s(&os["long_version"]), s(&os["arch"]));
    let up = v["uptime_secs"].as_u64().unwrap_or(0);
    println!("machine:  {}, user {}, up {}d {}h", s(&v["machine"]), s(&v["user"]), up / 86400, up % 86400 / 3600);
    let session = &v["session"];
    let mut traits = vec![format!("session {}", session["session_id"])];
    traits.push(if session["on_console"] == true { "console" } else { "not on the console" }.into());
    traits.push(if session["elevated"] == true { "elevated" } else { "not elevated" }.into());
    traits.push(match session["problem"].as_str() {
        Some(p) => format!("desktop not usable: {p}"),
        None => "desktop usable".into(),
    });
    println!("session:  {}", traits.join(", "));
    for m in v["monitors"].as_array().cloned().unwrap_or_default() {
        println!(
            "monitor:  #{} {}x{} at ({}, {}), scale {}{}",
            m["index"],
            m["width"],
            m["height"],
            m["x"],
            m["y"],
            m["scale_factor"],
            if m["primary"] == true { ", primary" } else { "" }
        );
    }
    println!("you:      \"{}\" with level {}", s(&v["you"]["peer_name"]), s(&v["you"]["level"]));
    if v["you"]["bypass"] == true {
        println!("bypass:   on. The owner trusts this device with everything: every tool, no approvals, no deny rules");
    }
    Ok(())
}

/// `ai-remote doctor --host`: the host's own report, printed like the local one.
pub async fn doctor(profile: &Profile, host: String, json_out: bool) -> Result<()> {
    let v = call(profile, &Target { host: Some(host) }, "doctor", json!({})).await?.ok();
    if json_out {
        print_json(&v);
    } else {
        let checks = v["checks"].as_array().cloned().unwrap_or_default();
        let width = checks.iter().filter_map(|c| c["id"].as_str()).map(str::len).max().unwrap_or(0);
        println!("Checked by the host:");
        for c in &checks {
            println!(
                "  {:<4}  {:<width$}  {}",
                c["grade"].as_str().unwrap_or("?"),
                c["id"].as_str().unwrap_or("?"),
                c["summary"].as_str().unwrap_or("")
            );
            if let Some(fix) = c["fix"].as_str() {
                println!("  {:<4}  {:<width$}  fix: {fix}", "", "");
            }
        }
        println!("Overall: {}", v["overall"].as_str().unwrap_or("?"));
    }
    if v["overall"] == "fail" {
        exit(1);
    }
    Ok(())
}

fn size(bytes: Option<u64>) -> String {
    let Some(b) = bytes else { return "-".into() };
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

pub async fn ls(profile: &Profile, a: LsArgs, json_out: bool) -> Result<()> {
    let mut entries = Vec::new();
    let mut offset = 0u64;
    let (path, total) = loop {
        let mut args = Map::new();
        args.insert("path".into(), a.path.clone().into());
        args.insert("offset".into(), offset.into());
        put(&mut args, "pattern", a.pattern.clone());
        let v = call(profile, &a.target, "files_list", Value::Object(args)).await?.ok();
        entries.extend(v["entries"].as_array().cloned().unwrap_or_default());
        match v["next_offset"].as_u64() {
            Some(next) if next > offset => offset = next,
            _ => break (v["path"].clone(), v["total"].clone()),
        }
    };
    if json_out {
        print_json(&json!({"path": path, "total": total, "entries": entries}));
        return Ok(());
    }
    for e in &entries {
        let modified = e["modified"].as_u64().map(|ms| local_time(Some(ms))).unwrap_or_else(|| "-".into());
        println!(
            "{:<5} {:>10}  {:<19}  {}",
            e["kind"].as_str().unwrap_or("?"),
            size(e["size"].as_u64()),
            modified,
            e["name"].as_str().unwrap_or("?")
        );
    }
    eprintln!("{} entries in {}", entries.len(), path.as_str().unwrap_or("?"));
    Ok(())
}

pub async fn ps(profile: &Profile, a: PsArgs, json_out: bool) -> Result<()> {
    let mut args = Map::new();
    put(&mut args, "name", a.name);
    put(&mut args, "pid", a.pid);
    put(&mut args, "limit", a.limit.map(|l| l as u64));
    let v = call(profile, &a.target, "process_list", Value::Object(args)).await?.ok();
    if json_out {
        print_json(&v);
        return Ok(());
    }
    println!("{:>8} {:>8} {:>10}  {:<28} EXE", "PID", "PPID", "MEM", "NAME");
    for p in v["processes"].as_array().cloned().unwrap_or_default() {
        println!(
            "{:>8} {:>8} {:>10}  {:<28} {}",
            p["pid"],
            p["parent_pid"].as_u64().map(|x| x.to_string()).unwrap_or_else(|| "-".into()),
            size(p["memory_bytes"].as_u64()),
            p["name"].as_str().unwrap_or("?"),
            p["exe"].as_str().unwrap_or("")
        );
    }
    if v["truncated"] == true {
        eprintln!("{} of {} shown; narrow it down with a name, or raise --limit", v["count"], v["total"]);
    }
    Ok(())
}

pub async fn kill(profile: &Profile, a: KillArgs, json_out: bool) -> Result<()> {
    let mut args = Map::new();
    put(&mut args, "pid", a.pid);
    put(&mut args, "name", a.name);
    put(&mut args, "intent", a.intent);
    if a.no_tree {
        args.insert("tree".into(), false.into());
    }
    let v = call(profile, &a.target, "process_kill", Value::Object(args)).await?.ok();
    if json_out {
        print_json(&v);
        return Ok(());
    }
    for p in v["killed"].as_array().cloned().unwrap_or_default() {
        println!("stopped {} {}", p["pid"], p["name"].as_str().unwrap_or(""));
    }
    println!("{}", v["result"].as_str().unwrap_or("?"));
    if !v["still_running"].as_array().is_none_or(Vec::is_empty) {
        std::process::exit(2);
    }
    Ok(())
}

pub async fn stat(profile: &Profile, a: PathArgs, json_out: bool) -> Result<()> {
    let v = call(profile, &a.target, "files_stat", json!({"path": a.path})).await?.ok();
    if json_out {
        print_json(&v);
        return Ok(());
    }
    let path = v["path"].as_str().unwrap_or("?");
    match v["exists"].as_bool() {
        Some(false) => println!("{path}: does not exist"),
        None => println!("{path}: cannot be read: {}", v["error"].as_str().unwrap_or("?")),
        Some(true) => {
            let mut line = format!("{path}: {}", v["kind"].as_str().unwrap_or("?"));
            if let Some(target) = v["target"].as_str() {
                line.push_str(&format!(" to {target} ({})", v["target_kind"].as_str().unwrap_or("missing")));
            }
            if v["size"].is_u64() {
                line.push_str(&format!(", {} ({} bytes)", size(v["size"].as_u64()), v["size"]));
            }
            line.push_str(&format!(", modified {}", local_time(v["modified"].as_u64())));
            if v["readonly"] == true {
                line.push_str(", read-only");
            }
            println!("{line}");
        }
    }
    Ok(())
}

pub async fn mkdir(profile: &Profile, a: MkdirArgs, json_out: bool) -> Result<()> {
    let mut args = Map::new();
    args.insert("path".into(), a.path.into());
    put(&mut args, "intent", a.intent);
    let v = call(profile, &a.target, "files_mkdir", Value::Object(args)).await?.ok();
    if json_out {
        print_json(&v);
    } else if v["created"] == true {
        println!("created {}", v["path"].as_str().unwrap_or("?"));
    } else {
        println!("{} already exists", v["path"].as_str().unwrap_or("?"));
    }
    Ok(())
}

pub async fn rm(profile: &Profile, a: RmArgs, json_out: bool) -> Result<()> {
    let mut args = Map::new();
    args.insert("path".into(), a.path.into());
    args.insert("recursive".into(), a.recursive.into());
    put(&mut args, "intent", a.intent);
    let v = call(profile, &a.target, "files_delete", Value::Object(args)).await?.ok();
    if json_out {
        print_json(&v);
    } else {
        println!("deleted {} ({})", v["path"].as_str().unwrap_or("?"), v["kind"].as_str().unwrap_or("?"));
    }
    Ok(())
}

/// The last component of a path written for either kind of system (`D:\cap\a.bin`, `/tmp/a.bin`).
fn remote_name(path: &str) -> Option<&str> {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty() && !n.ends_with(':'))
}

/// `dir` joined with `name`, using the separator `dir` already uses.
fn remote_join(dir: &str, name: &str) -> String {
    if dir.ends_with(['/', '\\']) {
        return format!("{dir}{name}");
    }
    let sep = if dir.contains('\\') || !dir.contains('/') && dir.contains(':') { '\\' } else { '/' };
    format!("{dir}{sep}{name}")
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Progress on stderr while a terminal shows it.
struct Progress {
    verb: &'static str,
    total: u64,
    done: u64,
    started: Instant,
    shown: Option<Instant>,
    visible: bool,
}

impl Progress {
    fn new(verb: &'static str, total: u64, quiet: bool) -> Self {
        let visible = !quiet && std::io::stderr().is_terminal();
        Self { verb, total, done: 0, started: Instant::now(), shown: None, visible }
    }

    fn add(&mut self, n: usize) {
        self.done += n as u64;
        if !self.visible || self.shown.is_some_and(|t| t.elapsed() < Duration::from_millis(200)) {
            return;
        }
        self.shown = Some(Instant::now());
        let pct = (self.done * 100).checked_div(self.total).unwrap_or(100);
        eprint!(
            "\r{} {pct:>3}%  {} of {}  {}/s   ",
            self.verb,
            size(Some(self.done)),
            size(Some(self.total)),
            self.rate()
        );
    }

    fn rate(&self) -> String {
        let secs = self.started.elapsed().as_secs_f64().max(0.001);
        size(Some((self.done as f64 / secs) as u64))
    }

    fn clear(&self) {
        if self.visible && self.shown.is_some() {
            eprint!("\r{:72}\r", "");
        }
    }
}

/// An error from redeeming a transfer ticket: the host's refusal (an expired or used ticket, a level
/// lowered meanwhile, a takeover) exits 2 like any refusal; anything else (unreachable) is returned.
pub(crate) fn refused_transfer(e: anyhow::Error) -> anyhow::Error {
    let text = format!("{e:#}");
    if text.starts_with("host '") {
        eprintln!("{text}");
        exit(2);
    }
    e
}

/// Redeems a transfer ticket through the agent: the host's first reply and the stream after it.
async fn open_transfer(
    profile: &Profile,
    target: &Target,
    ticket: &str,
) -> Result<(Value, impl AsyncRead + Unpin, impl tokio::io::AsyncWrite + Unpin)> {
    let mut conn = ar_controller::connect_agent(profile).await?;
    let reply = conn.request(&json!({"op": "files", "host": target.host, "request": {"ticket": ticket}})).await?;
    let (stream, buffered) = conn.into_raw();
    let (read, write) = tokio::io::split(stream);
    Ok((reply, std::io::Cursor::new(buffered).chain(read), write))
}

/// One JSON line from the host, after the bytes of a transfer.
async fn read_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Value> {
    let mut line = String::new();
    let mut reader = BufReader::new(reader.take(64 * 1024));
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        bail!("the host ended the transfer without an answer");
    }
    serde_json::from_str(&line).map_err(|e| anyhow!("bad answer from the host: {e}"))
}

pub async fn push(profile: &Profile, a: PushArgs, json_out: bool) -> Result<()> {
    let meta = std::fs::metadata(&a.local).with_context(|| format!("read {}", a.local.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file; push sends one file (pack a folder first, e.g. tar -czf)", a.local.display());
    }
    let name = a.local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // A folder on the host takes the file under its own name.
    let mut remote = a.remote.clone();
    if remote.ends_with(['/', '\\']) {
        remote = remote_join(&remote, &name);
    } else if let Ok(st) = call(profile, &a.target, "files_stat", json!({"path": remote})).await
        && !st.is_error()
    {
        let v = st.value();
        if v["kind"] == "dir" || v["target_kind"] == "dir" {
            remote = remote_join(&remote, &name);
        }
    }
    let local = a.local.clone();
    let sha256 = tokio::task::spawn_blocking(move || sha256_file(&local)).await??;
    let mut args = Map::new();
    args.insert("path".into(), remote.into());
    args.insert("size".into(), meta.len().into());
    args.insert("sha256".into(), sha256.clone().into());
    args.insert("overwrite".into(), a.force.into());
    args.insert("mkdir".into(), a.mkdir.into());
    put(&mut args, "intent", a.intent);
    let issued = call(profile, &a.target, "files_push", Value::Object(args)).await?.ok();
    let ticket = issued["ticket"].as_str().ok_or_else(|| anyhow!("the host issued no transfer ticket"))?;
    let (reply, mut reader, mut writer) = open_transfer(profile, &a.target, ticket).await.map_err(refused_transfer)?;
    let total = reply["size"].as_u64().unwrap_or(meta.len());
    let started = Instant::now();
    let mut progress = Progress::new("push", total, json_out);
    let mut file = tokio::fs::File::open(&a.local).await.with_context(|| format!("read {}", a.local.display()))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut sent = 0u64;
    while sent < total {
        let n = file.read(&mut buf).await.with_context(|| format!("read {}", a.local.display()))?;
        if n == 0 {
            progress.clear();
            bail!("{} shrank while it was sent", a.local.display());
        }
        let n = n.min((total - sent) as usize);
        if let Err(e) = writer.write_all(&buf[..n]).await {
            progress.clear();
            // The host stops reading when it fails; its answer says why.
            let answer = read_line(&mut reader).await.ok();
            let why = answer.as_ref().and_then(|v| v["error"].as_str()).map(str::to_string);
            eprintln!("{}", why.unwrap_or_else(|| format!("the transfer broke off: {e}")));
            exit(2);
        }
        sent += n as u64;
        progress.add(n);
    }
    writer.flush().await?;
    let answer = read_line(&mut reader).await;
    progress.clear();
    let answer = answer?;
    if answer["ok"] != true {
        eprintln!("{}", answer["error"].as_str().unwrap_or("the host refused the file"));
        exit(2);
    }
    let secs = started.elapsed().as_secs_f64();
    let summary = json!({
        "host": reply["host"],
        "local": a.local.display().to_string(),
        "remote": answer["path"].as_str().or(issued["path"].as_str()),
        "bytes": answer["bytes"],
        "sha256": answer["sha256"],
        "seconds": secs,
    });
    if json_out {
        print_json(&summary);
    } else {
        println!(
            "pushed {} to {}:{} in {secs:.1} s ({}/s), SHA-256 checked",
            size(answer["bytes"].as_u64()),
            reply["host"].as_str().unwrap_or("?"),
            summary["remote"].as_str().unwrap_or("?"),
            size(Some((total as f64 / secs.max(0.001)) as u64))
        );
    }
    Ok(())
}

pub async fn pull(profile: &Profile, a: PullArgs, json_out: bool) -> Result<()> {
    let name = remote_name(&a.remote).ok_or_else(|| anyhow!("'{}' names no file", a.remote))?.to_string();
    let dest = match a.local {
        None => PathBuf::from(&name),
        Some(p) if p.is_dir() || p.as_os_str().to_string_lossy().ends_with(['/', '\\']) => p.join(&name),
        Some(p) => p,
    };
    // Checked before asking the host, so a refusal here costs the owner no approval prompt.
    if dest.exists() && !a.force {
        bail!("{} exists; --force replaces it", dest.display());
    }
    let mut args = Map::new();
    args.insert("path".into(), a.remote.clone().into());
    put(&mut args, "intent", a.intent);
    let issued = call(profile, &a.target, "files_pull", Value::Object(args)).await?.ok();
    let ticket = issued["ticket"].as_str().ok_or_else(|| anyhow!("the host issued no transfer ticket"))?;
    let (reply, mut reader, _writer) = open_transfer(profile, &a.target, ticket).await.map_err(refused_transfer)?;
    let total = reply["size"].as_u64().ok_or_else(|| anyhow!("the host did not say how big the file is"))?;
    let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let part = dir.join(format!(".{}.ai-remote.part", dest.file_name().unwrap_or_default().to_string_lossy()));
    let started = Instant::now();
    let mut progress = Progress::new("pull", total, json_out);
    let received = receive_into(&part, &mut reader, total, &mut progress).await;
    progress.clear();
    let digest = match received {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            eprintln!("{e:#}");
            exit(2);
        }
    };
    let trailer = read_line(&mut reader).await;
    let expected = trailer.as_ref().ok().and_then(|v| v["sha256"].as_str()).map(str::to_string);
    if expected.as_deref() != Some(digest.as_str()) {
        let _ = std::fs::remove_file(&part);
        eprintln!(
            "the received file's SHA-256 ({digest}) does not match the host's ({}); nothing was saved",
            expected.as_deref().unwrap_or("missing")
        );
        exit(2);
    }
    std::fs::rename(&part, &dest).with_context(|| format!("save {}", dest.display()))?;
    let secs = started.elapsed().as_secs_f64();
    let summary = json!({
        "host": reply["host"],
        "remote": issued["path"],
        "local": dest.display().to_string(),
        "bytes": total,
        "sha256": digest,
        "seconds": secs,
    });
    if json_out {
        print_json(&summary);
    } else {
        println!(
            "pulled {} from {}:{} to {} in {secs:.1} s ({}/s), SHA-256 checked",
            size(Some(total)),
            reply["host"].as_str().unwrap_or("?"),
            issued["path"].as_str().unwrap_or("?"),
            dest.display(),
            size(Some((total as f64 / secs.max(0.001)) as u64))
        );
    }
    Ok(())
}

/// Reads exactly `total` bytes into `part` and returns their SHA-256.
async fn receive_into<R: AsyncRead + Unpin>(
    part: &Path,
    reader: &mut R,
    total: u64,
    progress: &mut Progress,
) -> Result<String> {
    let mut file = tokio::fs::File::create(part).await.with_context(|| format!("write {}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    while got < total {
        let want = (total - got).min(buf.len() as u64) as usize;
        let n = reader.read(&mut buf[..want]).await.map_err(|e| anyhow!("the transfer broke off: {e}"))?;
        if n == 0 {
            bail!(
                "the host stopped sending after {got} of {total} bytes (the file changed or could not be read there)"
            );
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await.with_context(|| format!("write {}", part.display()))?;
        got += n as u64;
        progress.add(n);
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_of_either_system() {
        assert_eq!(remote_name(r"D:\cap\a.ngfx-gputrace"), Some("a.ngfx-gputrace"));
        assert_eq!(remote_name("/tmp/logs/"), Some("logs"));
        assert_eq!(remote_name("C:\\"), None);
        assert_eq!(remote_join(r"D:\deploy", "build.zip"), r"D:\deploy\build.zip");
        assert_eq!(remote_join("D:/deploy/", "build.zip"), "D:/deploy/build.zip");
        assert_eq!(remote_join("/srv", "a"), "/srv/a");
        assert_eq!(remote_join("D:", "a"), r"D:\a");
    }

    #[test]
    fn env_pairs() {
        assert_eq!(env_map(&[]).unwrap(), None);
        let v = env_map(&["A=1".into(), "B=x=y".into(), "C=".into()]).unwrap().unwrap();
        assert_eq!(v, json!({"A": "1", "B": "x=y", "C": ""}));
        assert!(env_map(&["NOEQ".into()]).is_err());
        assert!(env_map(&["=v".into()]).is_err());
    }

    #[test]
    fn replies_give_their_value_and_extra_text() {
        let r = Reply(json!({"content": [{"type": "text", "text": "{\"a\":1}"}, {"type": "text", "text": "log"}]}));
        assert_eq!(r.value(), json!({"a": 1}));
        assert_eq!(r.second_text(), Some("log"));
        assert!(!r.is_error());
        let r =
            Reply(json!({"content": [{"type": "text", "text": "x"}], "structuredContent": {"b": 2}, "isError": false}));
        assert_eq!(r.value(), json!({"b": 2}));
    }

    #[test]
    fn sizes_and_durations_read_well() {
        assert_eq!(size(Some(512)), "512 B");
        assert_eq!(size(Some(3 * 1024 * 1024 / 2)), "1.5 MiB");
        assert_eq!(duration(Some(1500)), "1.5 s");
        assert_eq!(duration(Some(125_000)), "2m05s");
        assert_eq!(duration(Some(7_380_000)), "2h03m");
    }
}
