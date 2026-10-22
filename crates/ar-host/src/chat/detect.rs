//! Finding the coding agents installed here: where each program is, its version and whether it is
//! signed in.
//!
//! A program is looked for, in this order:
//!
//! 1. the path a custom entry names (a missing file is "not found", never a different program);
//! 2. the host's own `PATH` (with `PATHEXT` on Windows);
//! 3. the user's login shell (Unix): the background host is started by launchd or systemd with a
//!    minimal `PATH`, while version managers (nvm, fnm, volta), npm prefixes and installers add
//!    their directories in `~/.zshrc` and friends. One `$SHELL -ilc` resolves every name at once,
//!    after dropping aliases and functions of the same name (`claude` is often an alias), and also
//!    reports the shell's `PATH`: agents started from Node.js shims (`#!/usr/bin/env node`) need it
//!    too. Bounded by a timeout and cached;
//! 4. places installers use even when nothing adds them to `PATH`.
//!
//! The version comes from `--version` (the first token that looks like `1.2.3`), sign-in from the
//! agent's own status command where it has one (`claude auth status`, `codex login status`). Only
//! whether the agent is signed in and how (subscription or API key) leaves this host, never the
//! account itself.
//!
//! A host running as SYSTEM (Windows service mode) or root runs nothing here: that would run
//! programs from the user's writable directories with the host's privileges, and the agents would
//! not find the user's sign-in anyway.

use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::runtimes::{Caps, CustomAgent, Login, Protocol, RUNTIMES, Runtime, builtin};
use crate::platform;

/// How long one probe (`--version`, a sign-in check) may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the login shell may take to answer.
#[cfg(unix)]
const SHELL_TIMEOUT: Duration = Duration::from_secs(4);
/// How long a login-shell answer is reused while `PATH`, `SHELL` and `HOME` stay the same.
const SHELL_TTL: Duration = Duration::from_secs(30 * 60);
/// How long a full report is reused unless a refresh is asked for.
const REPORT_TTL: Duration = Duration::from_secs(60);

/// Why agents cannot start on this host, if they cannot.
pub fn blocked() -> Option<String> {
    if platform::runs_as_system() {
        return Some(
            "this host runs as SYSTEM (service mode); coding agents must run as the signed-in user, which a \
             SYSTEM host does not do yet. Run the host as the user (ai-remote host install) to use agents."
                .into(),
        );
    }
    if platform::runs_as_root() {
        return Some(
            "this host runs as root (the login window or a service install); coding agents refuse to run \
             unattended as root and would not find the user's sign-in."
                .into(),
        );
    }
    None
}

/// An agent ready to start: the program and how to call it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub id: String,
    pub name: String,
    pub runtime: &'static Runtime,
    /// What it can do: the runtime's caps, narrowed by a custom entry.
    pub caps: Caps,
    pub program: PathBuf,
    /// Arguments before the ones the adapter adds (a custom entry's).
    pub prefix: Vec<String>,
    /// The environment changes for the agent: the login shell's `PATH` and a custom entry's
    /// variables.
    pub env: BTreeMap<String, String>,
    pub resolved_by: &'static str,
}

/// Finds agents and remembers the answers for a while.
#[derive(Default)]
pub struct Detector {
    shell: Mutex<Option<ShellAnswer>>,
    report: Mutex<Option<(Instant, Value)>>,
}

#[derive(Clone)]
struct ShellAnswer {
    key: String,
    at: Instant,
    paths: HashMap<String, PathBuf>,
    path_var: Option<String>,
}

impl Detector {
    /// The installed agents, built in and custom, as shown to controllers.
    pub async fn report(&self, custom: &[CustomAgent], refresh: bool) -> Value {
        if let Some(why) = blocked() {
            return json!({"host": {"can_start": false, "reason": why}, "runtimes": []});
        }
        if !refresh
            && let Some((at, v)) = self.report.lock().unwrap().as_ref()
            && at.elapsed() < REPORT_TTL
        {
            return v.clone();
        }
        let mut entries: Vec<(String, String, &'static Runtime, Option<&CustomAgent>)> =
            RUNTIMES.iter().map(|r| (r.id.to_string(), r.name.to_string(), r, None)).collect();
        for c in custom {
            if let Some(base) = builtin(&c.base) {
                let name = c.name.clone().unwrap_or_else(|| format!("{} ({})", base.name, c.id));
                entries.push((c.id.clone(), name, base, Some(c)));
            }
        }
        let probes = entries.into_iter().map(|(id, name, runtime, custom)| {
            let found = self.locate(runtime, custom);
            let caps = custom.map(|c| c.caps(runtime)).unwrap_or(runtime.caps);
            async move { describe(id, name, runtime, caps, custom.is_some(), found).await }
        });
        let runtimes = futures::future::join_all(probes).await;
        let report = json!({"host": {"can_start": true, "reason": null}, "runtimes": runtimes});
        *self.report.lock().unwrap() = Some((Instant::now(), report.clone()));
        report
    }

    /// The agent `id` (built in, or custom from `custom`), ready to start.
    pub fn resolve(&self, id: &str, custom: &[CustomAgent]) -> Result<Resolved> {
        if let Some(why) = blocked() {
            bail!("{why}");
        }
        let (name, runtime, entry) = match custom.iter().find(|c| c.id == id) {
            Some(c) => {
                let base = builtin(&c.base).ok_or_else(|| anyhow::anyhow!("unknown base '{}'", c.base))?;
                (c.name.clone().unwrap_or_else(|| format!("{} ({})", base.name, c.id)), base, Some(c))
            }
            None => match builtin(id) {
                Some(r) => (r.name.to_string(), r, None),
                None => bail!(
                    "unknown agent '{id}'; known: {}",
                    RUNTIMES
                        .iter()
                        .map(|r| r.id)
                        .chain(custom.iter().map(|c| c.id.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
        };
        match self.locate(runtime, entry) {
            Ok(r) => Ok(Resolved { id: id.to_string(), name, ..r }),
            Err(why) => bail!("{name} is not installed on this host ({why})"),
        }
    }

    /// Where `runtime`'s program is (or the custom entry's), with the environment to run it in.
    fn locate(&self, runtime: &'static Runtime, custom: Option<&CustomAgent>) -> Result<Resolved, String> {
        let mut env = BTreeMap::new();
        if let Some(c) = custom {
            for (k, v) in &c.env {
                env.insert(k.clone(), expand_home(v));
            }
        }
        let shell = self.shell_answer();
        if let Some(p) = shell.as_ref().and_then(|s| s.path_var.clone()) {
            env.entry("PATH".into()).or_insert(p);
        }
        let done = |program: PathBuf, by: &'static str, env: BTreeMap<String, String>| Resolved {
            id: runtime.id.to_string(),
            name: runtime.name.to_string(),
            runtime,
            caps: custom.map(|c| c.caps(runtime)).unwrap_or(runtime.caps),
            program,
            prefix: custom.map(|c| c.args.clone()).unwrap_or_default(),
            env,
            resolved_by: by,
        };
        if let Some(cmd) = custom.and_then(|c| c.command.as_deref()) {
            let cmd = expand_home(cmd);
            if cmd.contains('/') || cmd.contains('\\') {
                let p = PathBuf::from(&cmd);
                return if is_program(&p) {
                    Ok(done(p, "configured", env))
                } else {
                    Err(format!("{cmd} does not exist"))
                };
            }
            return find(&cmd, shell.as_ref())
                .map(|(p, by)| done(p, by, env))
                .ok_or_else(|| format!("{cmd} not found"));
        }
        for name in runtime.commands {
            if let Some((p, by)) = find(name, shell.as_ref()) {
                return Ok(done(p, by, env));
            }
        }
        Err(format!("{} not found", runtime.commands.join(" / ")))
    }

    /// The login shell's answer, asked at most once per [`SHELL_TTL`] (Unix only).
    fn shell_answer(&self) -> Option<ShellAnswer> {
        if cfg!(windows) {
            return None;
        }
        let key = ["PATH", "SHELL", "HOME"].map(|k| std::env::var(k).unwrap_or_default()).join("\0");
        let mut cached = self.shell.lock().unwrap();
        if let Some(a) = cached.as_ref().filter(|a| a.key == key && a.at.elapsed() < SHELL_TTL) {
            return Some(a.clone());
        }
        let names: Vec<&str> = RUNTIMES.iter().flat_map(|r| r.commands.iter().copied()).collect();
        let (paths, path_var) = ask_login_shell(&names).unwrap_or_default();
        let answer = ShellAnswer { key, at: Instant::now(), paths, path_var };
        *cached = Some(answer.clone());
        Some(answer)
    }
}

/// `program` on the host's `PATH`, in the login shell's answer or in an installer's usual place.
fn find(name: &str, shell: Option<&ShellAnswer>) -> Option<(PathBuf, &'static str)> {
    if let Some(p) = which(name, std::env::var_os("PATH")) {
        return Some((p, "path"));
    }
    if let Some(s) = shell {
        if let Some(p) = s.paths.get(name).filter(|p| is_program(p)) {
            return Some((p.clone(), "login_shell"));
        }
        if let Some(p) = which(name, s.path_var.clone().map(OsString::from)) {
            return Some((p, "login_shell"));
        }
    }
    known_places().into_iter().find_map(|dir| which(name, Some(dir.into_os_string()))).map(|p| (p, "known_place"))
}

/// Looks `name` up in the directories of `path` (a `PATH` value).
fn which(name: &str, path: Option<OsString>) -> Option<PathBuf> {
    let path = path?;
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        // A native program first: an npm `.cmd` shim re-parses its arguments through cmd.exe.
        let mut v: Vec<String> = pathext.split(';').filter(|e| !e.is_empty()).map(|e| e.to_ascii_lowercase()).collect();
        v.sort_by_key(|e| if e == ".exe" { 0 } else { 1 });
        v
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if is_program(&p) {
                return Some(p);
            }
        }
    }
    None
}

fn is_program(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else { return false };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `~` or `~/...` at the start of `s` with the home directory.
pub fn expand_home(s: &str) -> String {
    match (s.strip_prefix('~'), home()) {
        (Some(rest), Some(h)) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            format!("{}{rest}", h.display())
        }
        _ => s.to_string(),
    }
}

/// Directories installers use whether or not they end up on `PATH`.
fn known_places() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let h = home();
    if cfg!(windows) {
        if let Some(h) = &h {
            v.push(h.join(".local").join("bin"));
        }
        if let Some(a) = std::env::var_os("APPDATA") {
            v.push(PathBuf::from(a).join("npm"));
        }
    } else {
        if let Some(h) = &h {
            for d in [".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".opencode/bin", ".cargo/bin"] {
                v.push(h.join(d));
            }
        }
        v.push("/opt/homebrew/bin".into());
        v.push("/usr/local/bin".into());
    }
    v
}

/// Asks the user's login shell where each of `names` is, and for its `PATH`.
#[cfg(unix)]
fn ask_login_shell(names: &[&str]) -> Option<(HashMap<String, PathBuf>, Option<String>)> {
    use std::io::Read;
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh".into() } else { "/bin/bash".into() });
    let base = Path::new(&shell).file_name()?.to_string_lossy().into_owned();
    // fish and others speak another language; their users' PATH usually reaches the host anyway.
    if !["bash", "zsh", "sh", "dash", "ksh"].contains(&base.as_str()) {
        return None;
    }
    let mut script = String::new();
    for n in names {
        script.push_str(&format!(
            "unalias {n} 2>/dev/null; unset -f {n} 2>/dev/null; p=$(command -v {n} 2>/dev/null) && \
             case \"$p\" in /*) printf '%s\\t%s\\n' {n} \"$p\";; esac; "
        ));
    }
    script.push_str("printf '__PATH__\\t%s\\n' \"$PATH\"");
    let mut child = std::process::Command::new(&shell)
        .args(["-ilc", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let text = match rx.recv_timeout(SHELL_TIMEOUT) {
        Ok(s) => s,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!(%shell, "the login shell did not answer in time; agents installed through it may not be found");
            return None;
        }
    };
    let _ = child.wait();
    let mut paths = HashMap::new();
    let mut path_var = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('\t') else { continue };
        if k == "__PATH__" {
            path_var = Some(v.trim().to_string()).filter(|v| !v.is_empty());
        } else if names.contains(&k) && v.starts_with('/') {
            // Symlinks are kept as they are: an installer that updates itself moves the link's
            // target, and the link is resolved again at each start.
            paths.insert(k.to_string(), PathBuf::from(v.trim()));
        }
    }
    Some((paths, path_var))
}

#[cfg(not(unix))]
fn ask_login_shell(_names: &[&str]) -> Option<(HashMap<String, PathBuf>, Option<String>)> {
    None
}

/// One row of the report.
async fn describe(
    id: String,
    name: String,
    runtime: &'static Runtime,
    caps: Caps,
    custom: bool,
    found: Result<Resolved, String>,
) -> Value {
    let base = json!({
        "id": id,
        "name": name,
        "protocol": runtime.protocol,
        "base": runtime.id,
        "custom": custom,
        "caps": caps,
    });
    let mut row = base.as_object().cloned().unwrap_or_default();
    let r = match found {
        Ok(r) => r,
        Err(why) => {
            row.insert("verdict".into(), json!("missing"));
            row.insert("detail".into(), json!(why));
            return Value::Object(row);
        }
    };
    row.insert("path".into(), json!(r.program.display().to_string()));
    row.insert("resolved_by".into(), json!(r.resolved_by));
    let version = probe(&r, &["--version"]).await;
    let (verdict, detail, version) = match version {
        Probe::Ran { out, .. } => match parse_version(&out) {
            Some(v) => match runtime.min_version.filter(|min| older(&v, min)) {
                Some(min) => ("below_minimum", Some(format!("version {v} is older than {min}; update it")), Some(v)),
                None => ("ok", None, Some(v)),
            },
            None => ("ok", Some("version unknown".to_string()), None),
        },
        Probe::Failed(why) => ("not_executable", Some(why), None),
    };
    row.insert("verdict".into(), json!(verdict));
    row.insert("version".into(), json!(version));
    row.insert("detail".into(), json!(detail));
    if verdict != "not_executable" {
        row.insert("login".into(), login_state(&r).await);
    }
    Value::Object(row)
}

enum Probe {
    Ran { code: Option<i32>, out: String },
    Failed(String),
}

/// Runs the agent with `args` and its environment, briefly.
async fn probe(r: &Resolved, args: &[&str]) -> Probe {
    let mut cmd = tokio::process::Command::new(&r.program);
    cmd.args(&r.prefix).args(args).envs(&r.env).stdin(std::process::Stdio::null()).kill_on_drop(true);
    super::proc::scrub_env(&mut cmd);
    platform::no_console_window(cmd.as_std_mut());
    match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => {
            let mut out = String::from_utf8_lossy(&o.stdout).into_owned();
            out.push('\n');
            out.push_str(&String::from_utf8_lossy(&o.stderr));
            Probe::Ran { code: o.status.code(), out }
        }
        Ok(Err(e)) => Probe::Failed(format!("cannot run {}: {e}", r.program.display())),
        Err(_) => Probe::Failed(format!("{} did not answer within {} s", r.program.display(), PROBE_TIMEOUT.as_secs())),
    }
}

/// Whether the agent is signed in, as far as it says.
async fn login_state(r: &Resolved) -> Value {
    let args = match r.runtime.login {
        Login::NotChecked => return Value::Null,
        Login::DeepSeek => return deepseek_login(r),
        Login::Command(args) => args,
    };
    if r.runtime.protocol == Protocol::ClaudeStreamJson {
        let mut a = args.to_vec();
        a.push("--json");
        if let Probe::Ran { out, .. } = probe(r, &a).await {
            let start = out.find('{');
            if let Some(v) = start.and_then(|s| serde_json::from_str::<Value>(out[s..].trim()).ok()) {
                let state = if v["loggedIn"].as_bool() == Some(true) { "logged_in" } else { "logged_out" };
                return json!({"state": state, "method": v["authMethod"], "plan": v["subscriptionType"]});
            }
        }
        return json!({"state": "unknown"});
    }
    match probe(r, args).await {
        Probe::Ran { code: Some(0), out } => {
            let method = if out.contains("ChatGPT") {
                Some("chatgpt")
            } else if out.to_lowercase().contains("api key") {
                Some("api_key")
            } else {
                None
            };
            json!({"state": "logged_in", "method": method})
        }
        Probe::Ran { .. } => json!({"state": "logged_out"}),
        Probe::Failed(_) => json!({"state": "unknown"}),
    }
}

/// DeepSeek Harness has no status command. It reads `DEEPSEEK_API_KEY` from its environment, its
/// credential file (`$DSH_HOME/.credentials.yaml`, `~/.dsh` by default, which also holds a
/// DeepSeek account sign-in) and `$DSH_HOME/.env`. Only whether a key or account is there is
/// looked at, never its value. Nothing there but a harness home: another provider may be set up,
/// so unknown.
fn deepseek_login(r: &Resolved) -> Value {
    let var = |k: &str| r.env.get(k).cloned().or_else(|| std::env::var(k).ok()).filter(|v| !v.trim().is_empty());
    if var("DEEPSEEK_API_KEY").is_some() {
        return json!({"state": "logged_in", "method": "api_key"});
    }
    let dsh_home = match var("DSH_HOME") {
        Some(h) => PathBuf::from(expand_home(&h)),
        None => match home() {
            Some(h) => h.join(".dsh"),
            None => return json!({"state": "unknown"}),
        },
    };
    deepseek_files(&dsh_home)
}

fn deepseek_files(dsh_home: &Path) -> Value {
    if !dsh_home.is_dir() {
        return json!({"state": "logged_out"});
    }
    let read = |f: &str| std::fs::read_to_string(dsh_home.join(f)).unwrap_or_default();
    let credentials = read(".credentials.yaml");
    if credentials.contains("deepseek-account") {
        return json!({"state": "logged_in", "method": "account"});
    }
    let has_key = |text: &str, sep: char| {
        text.lines().any(|l| {
            let l = l.trim().trim_start_matches("export ");
            l.strip_prefix("DEEPSEEK_API_KEY")
                .and_then(|rest| rest.trim_start().strip_prefix(sep))
                .is_some_and(|v| !v.trim().trim_matches(['"', '\'']).is_empty())
        })
    };
    if has_key(&credentials, ':') || has_key(&read(".env"), '=') {
        return json!({"state": "logged_in", "method": "api_key"});
    }
    json!({"state": "unknown"})
}

/// The first `major.minor.patch` in `text`.
pub fn parse_version(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',').find_map(|tok| {
        let tok = tok.trim_start_matches('v');
        let core: String = tok.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let parts: Vec<&str> = core.split('.').filter(|p| !p.is_empty()).collect();
        (parts.len() >= 3 && parts.iter().all(|p| p.parse::<u64>().is_ok())).then(|| parts[..3].join("."))
    })
}

/// Whether version `v` is older than `min` (both `a.b.c`).
fn older(v: &str, min: &str) -> bool {
    let n = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    n(v) < n(min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_found_in_noisy_output() {
        assert_eq!(parse_version("2.1.174 (Claude Code)"), Some("2.1.174".into()));
        assert_eq!(parse_version("codex-cli 0.137.0"), Some("0.137.0".into()));
        assert_eq!(parse_version("Active code page: 65001\nv1.2.3-beta"), Some("1.2.3".into()));
        assert_eq!(parse_version("version 1.2"), None);
        assert!(older("0.99.9", "0.100.0") && !older("0.100.0", "0.100.0") && !older("2.1.174", "2.0.0"));
    }

    #[test]
    fn deepseek_keys_and_accounts_are_found_without_reading_values_out() {
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().join(".dsh");
        assert_eq!(deepseek_files(&h)["state"], "logged_out", "no harness home: never set up");
        std::fs::create_dir(&h).unwrap();
        assert_eq!(deepseek_files(&h)["state"], "unknown");
        std::fs::write(h.join(".credentials.yaml"), "refs:\n  DEEPSEEK_API_KEY: ''\n").unwrap();
        assert_eq!(deepseek_files(&h)["state"], "unknown", "an empty key is no key");
        std::fs::write(h.join(".env"), "export DEEPSEEK_API_KEY=sk-x\n").unwrap();
        let v = deepseek_files(&h);
        assert_eq!((v["state"].as_str(), v["method"].as_str()), (Some("logged_in"), Some("api_key")));
        assert!(!v.to_string().contains("sk-x"));
        std::fs::write(h.join(".credentials.yaml"), "records:\n  deepseek-account/default:\n    grant: {}\n").unwrap();
        assert_eq!(deepseek_files(&h)["method"], "account");
    }

    #[test]
    fn home_is_expanded_only_at_the_start() {
        let h = home().unwrap();
        assert_eq!(expand_home("~/.claude-work"), format!("{}/.claude-work", h.display()));
        assert_eq!(expand_home("~"), h.display().to_string());
        assert_eq!(expand_home("a/~b"), "a/~b");
        assert_eq!(expand_home("~other"), "~other");
    }

    #[cfg(unix)]
    #[test]
    fn programs_are_found_on_a_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fake-agent");
        std::fs::write(&p, "#!/bin/sh\necho 'fake 1.2.3'\n").unwrap();
        assert!(which("fake-agent", Some(dir.path().as_os_str().to_owned())).is_none(), "not executable yet");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(which("fake-agent", Some(dir.path().as_os_str().to_owned())), Some(p));
        assert!(which("nope", Some(dir.path().as_os_str().to_owned())).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_custom_entry_runs_with_its_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("claude");
        std::fs::write(
            &p,
            "#!/bin/sh\nif [ \"$1\" = auth ]; then echo \"{\\\"loggedIn\\\": true, \\\"authMethod\\\": \\\"$WHO\\\"}\"; \
             else echo '9.9.9 (Claude Code)'; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let custom = CustomAgent {
            id: "claude-work".into(),
            base: "claude".into(),
            name: None,
            command: Some(p.display().to_string()),
            args: vec![],
            env: [("WHO".to_string(), "work".to_string())].into(),
            steer: None,
        };
        let d = Detector::default();
        let r = d.resolve("claude-work", std::slice::from_ref(&custom)).unwrap();
        assert_eq!((r.program.as_path(), r.resolved_by, r.runtime.id), (p.as_path(), "configured", "claude"));
        let row = describe(r.id.clone(), r.name.clone(), r.runtime, r.caps, true, Ok(r)).await;
        assert_eq!(row["version"], "9.9.9");
        assert_eq!(row["verdict"], "ok");
        assert_eq!(row["login"]["state"], "logged_in");
        assert_eq!(row["login"]["method"], "work");
        let missing = CustomAgent { command: Some("/nonexistent/claude".into()), ..custom };
        assert!(d.resolve("claude-work", &[missing]).is_err());
    }
}
