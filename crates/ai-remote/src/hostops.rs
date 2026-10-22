//! `ai-remote host install | uninstall | status` and `ai-remote doctor`: running the host as a
//! background program started at login, and checking that a machine can really be operated.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use ar_core::{Profile, Role};
use ar_host::{
    HostRequest,
    doctor::{self, Grade, Report},
    install::{self, Action, Plan, Scope},
};
use ar_net::ipc::Conn;
use serde_json::{Value, json};

/// The running host's `status` answer, if a host runs for this profile.
async fn running_host(profile: &Profile) -> Option<Value> {
    let mut conn = Conn::connect(&profile.control_endpoint(Role::Host).ok()?).await.ok()?;
    conn.request(&serde_json::to_value(HostRequest::Status).ok()?).await.ok()
}

async fn wait_for_host(profile: &Profile, timeout: Duration) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(status) = running_host(profile).await {
            return Some(status);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Flags of `ai-remote host install`.
pub struct InstallFlags {
    pub dry_run: bool,
    pub no_start: bool,
    pub firewall_only: bool,
    pub force: bool,
    /// Windows: a service running the host as SYSTEM in the console session (lock screen, UAC).
    pub service: bool,
    /// With `dry_run`: print the plan as JSON (the desktop app shows it before it asks for an
    /// administrator).
    pub json: bool,
}

/// A plan as the desktop app shows it.
fn plan_json(plan: &Plan) -> Value {
    let steps: Vec<Value> =
        plan.actions.iter().map(|a| json!({"text": a.describe(), "admin": a.needs_admin()})).collect();
    json!({
        "summary": plan.summary,
        "notes": plan.notes,
        "steps": steps,
        "needs_elevation": plan.needs_elevation,
        "blocker": plan.blocker,
    })
}

pub async fn host_install(profile: &Profile, flags: InstallFlags) -> Result<()> {
    let scope = if flags.firewall_only { Scope::FirewallOnly } else { Scope::All };
    let p = profile.clone();
    let service = flags.service;
    let plan = tokio::task::spawn_blocking(move || install::install_plan(&p, scope, service)).await??;
    if flags.dry_run && flags.json {
        println!("{}", serde_json::to_string_pretty(&plan_json(&plan))?);
        return Ok(());
    }
    print_plan(&plan, flags.dry_run, flags.service);
    if let Some(blocker) = &plan.blocker {
        if !flags.dry_run && !flags.force {
            bail!("{blocker}");
        }
        println!("Warning: {blocker}\n");
    }
    if flags.dry_run {
        return Ok(());
    }
    let with_admin = admin_steps_allowed(&plan, profile, "install", scope, flags.service)?;
    if scope == Scope::All {
        // Installing means "run it": a stop recorded by `stop-host` must not keep the agent the
        // plan loads from starting.
        clear_stop_marker(profile);
    }
    execute(&plan, with_admin)?;
    if scope == Scope::FirewallOnly {
        println!("\nThe firewall rules are in place.");
        return Ok(());
    }
    if flags.service && cfg!(windows) {
        println!("\nThe service is installed. The host it starts runs as SYSTEM: see the notes above.");
    } else if flags.service {
        println!("\nThe system LaunchAgent is installed: the host also serves the login window. See the notes above.");
    } else {
        println!("\nAutostart is installed.");
    }
    if !with_admin && plan.has_admin_steps() {
        println!(
            "\nThe firewall rules need an administrator. Ask one to run this from an elevated terminal (any \
             administrator account):\n\n  {} host install --firewall-only\n\nUntil then Windows asks on the desktop \
             whether to allow the host at its first incoming connection; an unanswered prompt blocks it.",
            this_exe()
        );
    }
    if plan.blocker.is_some() {
        println!(
            "It starts when this account logs on; it is not started now because the desktop belongs to another account."
        );
        return Ok(());
    }
    if flags.no_start {
        return Ok(());
    }
    if let Some(host) = running_host(profile).await {
        println!(
            "A host is already running for this profile (pid {}); the background host takes over at the next login. \
             To switch now, stop it (`ai-remote stop-host`) and run `ai-remote host install` again.",
            host["pid"]
        );
        return Ok(());
    }
    // `sudo ai-remote host install --service` on macOS: launchd has started the host as the
    // owner already, and root may not talk to the owner's control socket (cross-uid refusal).
    #[cfg(target_os = "macos")]
    if ar_host::platform::runs_as_root() {
        println!(
            "\nlaunchd starts the host as the installing user; check it without sudo:\n\n  ai-remote host status\n  \
             ai-remote doctor\n\nThen grant Screen Recording and Accessibility to ai-remote-hostw if doctor says they are \
             missing, and restart the host (`launchctl kickstart -k gui/<uid>/<label>`, or log out and in; not \
             `ai-remote stop-host`, whose stop holds until the next login)."
        );
        return Ok(());
    }
    println!("\nStarting the background host...");
    let start = install::start_action(profile)?;
    if let Some(note) = start.execute()? {
        println!("  {note}");
    }
    let log = profile.log_path(Role::Host)?;
    let Some(host) = wait_for_host(profile, Duration::from_secs(20)).await else {
        bail!("the background host did not answer within 20 s; see {}", log.display());
    };
    println!("The background host is running (pid {}); it logs to {}.\n", host["pid"], log.display());
    // The self-check: the host's own view of its session, token and network.
    print_report(&host_doctor(profile).await?);
    if host["peers"].as_u64() == Some(0) {
        println!("\nNext: pair a controller. Run `ai-remote pair-code` here and follow its instructions.");
    }
    Ok(())
}

pub async fn host_uninstall(profile: &Profile, dry_run: bool, firewall_only: bool) -> Result<()> {
    let scope = if firewall_only { Scope::FirewallOnly } else { Scope::All };
    let p = profile.clone();
    let plan = tokio::task::spawn_blocking(move || install::uninstall_plan(&p, scope)).await??;
    let running = if scope == Scope::All { running_host(profile).await } else { None };
    print_plan(&plan, dry_run, false);
    if let Some(host) = &running {
        println!("  - Also stops the running host (pid {}).", host["pid"]);
    }
    if dry_run {
        return Ok(());
    }
    let with_admin = admin_steps_allowed(&plan, profile, "uninstall", scope, false)?;
    if running.is_some() {
        stop_host(profile).await?;
    }
    execute(&plan, with_admin)?;
    if scope == Scope::FirewallOnly {
        println!("\nDone.");
        return Ok(());
    }
    clear_stop_marker(profile);
    println!("\nAutostart is removed.");
    if !with_admin && plan.has_admin_steps() {
        println!(
            "The firewall rules stay: removing them needs an administrator. One can run, from an elevated terminal:\n\n  \
             {} host uninstall --firewall-only",
            this_exe()
        );
    }
    Ok(())
}

/// This installation's CLI by full path, quoted, for commands another account (an administrator)
/// should run: the firewall rules name these very programs.
fn this_exe() -> String {
    install::Binaries::locate()
        .map(|b| install::quote_arg(&b.cli.to_string_lossy()))
        .unwrap_or_else(|_| "ai-remote".into())
}

/// Asks the host to stop and waits until it no longer answers.
async fn stop_host(profile: &Profile) -> Result<()> {
    let endpoint = profile.control_endpoint(Role::Host)?;
    if let Ok(mut conn) = Conn::connect(&endpoint).await {
        let _ = conn.request(&serde_json::to_value(HostRequest::Shutdown)?).await;
    }
    for _ in 0..40 {
        if running_host(profile).await.is_none() {
            println!("  stopped the running host");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!("  the running host did not stop within 10 s; the task stop below ends it");
    Ok(())
}

/// `ai-remote host start`: starts the installed background host (launchctl kickstart, the logon
/// task or the service) and waits until it answers on its control channel.
pub async fn host_start(profile: &Profile, json_out: bool) -> Result<()> {
    if let Some(host) = running_host(profile).await {
        if json_out {
            println!("{}", serde_json::to_string_pretty(&json!({"started": false, "host": host}))?);
        } else {
            println!("A host is already running for this profile (pid {}).", host["pid"]);
        }
        return Ok(());
    }
    let p = profile.clone();
    let status = tokio::task::spawn_blocking(move || install::status(&p)).await??;
    if !status.installed {
        bail!("no background host is installed for this profile: run `ai-remote host install` first");
    }
    // Starting is the owner's answer to an earlier `stop-host`: without this the macOS system
    // agent would find the stop still recorded and exit again at once.
    clear_stop_marker(profile);
    let start = install::start_action(profile)?;
    // A Windows service starts only for an administrator (its default access rights).
    if start.needs_admin() && ar_host::platform::rights() != ar_host::platform::Rights::Elevated {
        bail!(
            "this host is a Windows service ({}), which only an administrator can start: run `ai-remote host start` \
             in a terminal opened as administrator, or restart the computer",
            status.name
        );
    }
    if let Some(note) = start.execute()?
        && !json_out
    {
        println!("  {note}");
    }
    let log = profile.log_path(Role::Host)?;
    let Some(host) = wait_for_host(profile, Duration::from_secs(20)).await else {
        bail!("the background host did not answer within 20 s; see {}", log.display());
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({"started": true, "host": host}))?);
    } else {
        println!("The background host is running (pid {}); it logs to {}.", host["pid"], log.display());
    }
    Ok(())
}

pub async fn host_status(profile: &Profile, json_out: bool) -> Result<()> {
    let p = profile.clone();
    let status = tokio::task::spawn_blocking(move || install::status(&p)).await?;
    let host = running_host(profile).await;
    #[cfg(target_os = "macos")]
    let (running, system_agent) =
        (host.is_some(), matches!(&status, Ok(s) if s.installed && s.kind.contains("login window")));
    if json_out {
        let autostart = match &status {
            Ok(s) => serde_json::to_value(s)?,
            Err(e) => json!({"error": format!("{e:#}")}),
        };
        println!("{}", serde_json::to_string_pretty(&json!({"autostart": autostart, "host": host}))?);
        return Ok(());
    }
    match status {
        Ok(s) => {
            if s.installed {
                let running = match s.running {
                    Some(true) => ", running",
                    Some(false) => ", not running",
                    None => "",
                };
                println!("autostart: {} {} is installed{running}", s.kind, s.name);
                if let Some(cmd) = &s.launches {
                    println!("           runs {cmd}");
                }
                if s.current == Some(false) {
                    println!("           not this installation or profile: run `ai-remote host install` again");
                }
            } else {
                let why = s.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
                println!("autostart: not installed{why}");
            }
            for fw in &s.firewall {
                let allow = if fw.allow.is_empty() {
                    "no allow rule".to_string()
                } else {
                    format!("allow {}", fw.allow.join(", "))
                };
                let block = if fw.block.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; BLOCKED by {}",
                        fw.block.iter().map(|b| format!("\"{b}\"")).collect::<Vec<_>>().join(", ")
                    )
                };
                println!("firewall:  {}: {allow}{block}", fw.program);
            }
            if let Some(fw) = s.firewall.first().filter(|fw| !fw.block_all.is_empty()) {
                println!(
                    "firewall:  inbound UDP of every program is blocked by {} (beats the allow rules; review in wf.msc)",
                    fw.block_all.iter().map(|b| format!("\"{b}\"")).collect::<Vec<_>>().join(", ")
                );
            }
            if let Some(e) = &s.firewall_error {
                println!("firewall:  unknown ({e})");
            }
            if s.firewall_enabled == Some(false) {
                println!("firewall:  Windows Firewall is off on every network profile; the rules matter once it is on");
            }
        }
        Err(e) => println!("autostart: unknown ({e:#})"),
    }
    match host {
        Some(h) => println!(
            "host:      running as \"{}\" (pid {}, {})",
            h["name"].as_str().unwrap_or("?"),
            h["pid"],
            if h["background"].as_bool() == Some(true) { "background" } else { "in a terminal" }
        ),
        None => println!("host:      not running"),
    }
    #[cfg(target_os = "macos")]
    print_handoff_state(profile, running, system_agent);
    Ok(())
}

/// Removes the record `stop-host` leaves for the macOS system agent (see `ar_host::handoff`).
fn clear_stop_marker(profile: &Profile) {
    if let Ok(path) = ar_host::handoff::marker_path(profile)
        && let Err(e) = ar_host::handoff::clear_marker(&path)
    {
        println!("Warning: {e:#}");
    }
}

/// macOS system agent: a stop that still holds, and where a waiting instance says why it waits.
#[cfg(target_os = "macos")]
fn print_handoff_state(profile: &Profile, running: bool, system_agent: bool) {
    use ar_host::{handoff, platform::macos};
    let marker = handoff::marker_path(profile).ok().and_then(|p| handoff::read_marker(&p).ok().flatten());
    let this_boot = marker.as_ref().is_some_and(|m| macos::boot_session().as_deref() == Some(m.boot.as_str()));
    if running || !system_agent {
        return;
    }
    if this_boot && marker.as_ref().is_some_and(|m| m.login_window) {
        println!(
            "           stopped with `ai-remote stop-host` at the login window: the system agent stays stopped in every \
             session until a restart (or `host install`)"
        );
    } else if this_boot {
        println!(
            "           stopped with `ai-remote stop-host`: the system agent stays stopped in this login session and at \
             the login window until the next login or restart (or `host install`)"
        );
    } else if let Ok(log) = profile.log_path(Role::Host) {
        println!(
            "           a system agent that waits (its session is not on the console, or another host holds the \
             profile) says so in {}",
            log.display()
        );
    }
}

/// The running host's doctor report.
async fn host_doctor(profile: &Profile) -> Result<Report> {
    let mut conn = Conn::connect(&profile.control_endpoint(Role::Host)?).await.context("connect to the host")?;
    let v = conn.request(&serde_json::to_value(HostRequest::Doctor)?).await?;
    serde_json::from_value(v).context("unexpected doctor report from the host")
}

pub async fn doctor(profile: &Profile, json_out: bool) -> Result<()> {
    let report = if running_host(profile).await.is_some() {
        host_doctor(profile).await?
    } else {
        // Same process setup as the host, so the DPI check measures what the host would get.
        ar_host::platform::init_process();
        doctor::run(doctor::Inputs { profile, mesh: None }).await
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    if report.overall == Grade::Fail {
        std::process::exit(1);
    }
    Ok(())
}

fn print_report(report: &Report) {
    if report.checked_in == "host" {
        println!("Checked by the running host:");
    } else {
        println!("No host is running; checked from this terminal:");
    }
    let width = report.checks.iter().map(|c| c.id.len()).max().unwrap_or(0);
    for c in &report.checks {
        println!("  {:<4}  {:<width$}  {}", c.grade.as_str(), c.id, c.summary);
        if let Some(fix) = &c.fix {
            println!("  {:<4}  {:<width$}  fix: {fix}", "", "");
        }
    }
    println!("Overall: {}", report.overall.as_str());
}

fn print_plan(plan: &Plan, dry_run: bool, service: bool) {
    println!("{}", plan.summary);
    for note in &plan.notes {
        println!("  - {note}");
    }
    if !dry_run {
        println!();
        return;
    }
    println!("\nSteps (dry run, nothing is changed):");
    for (i, action) in plan.actions.iter().enumerate() {
        println!("  {}. {}", i + 1, action.describe().replace('\n', "\n   "));
        if let Action::WriteFile { preview, .. } = action {
            for line in preview.lines() {
                println!("       | {line}");
            }
        }
    }
    if plan.needs_elevation && service && cfg!(target_os = "macos") {
        println!("\nRun it without --dry-run to apply, with sudo: /Library/LaunchAgents belongs to root.");
    } else if plan.needs_elevation && service {
        println!("\nRun it without --dry-run to apply, from an elevated terminal: a service is administrator work.");
    } else if plan.needs_elevation {
        println!(
            "\nRun it without --dry-run to apply: from an elevated terminal if this account is an administrator. A \
             standard user's run does the rest and leaves the steps marked \"administrator\" to `--firewall-only` \
             in an administrator's elevated terminal."
        );
    }
}

/// Whether this process runs the plan's administrator steps too (`Ok(false)`: a standard user
/// runs the rest). An administrator who is not elevated is sent to an elevated terminal: their
/// logon task needs it for "highest privileges", and so do the firewall rules. A service is
/// administrator work from the first step to the last.
fn admin_steps_allowed(plan: &Plan, profile: &Profile, verb: &str, scope: Scope, service: bool) -> Result<bool> {
    use ar_host::platform::Rights;
    if !plan.needs_elevation {
        return Ok(true);
    }
    let firewall_flag = if scope == Scope::FirewallOnly { " --firewall-only" } else { "" };
    match ar_host::platform::rights() {
        Rights::Elevated => Ok(true),
        Rights::StandardUser if service => bail!(
            "a service is registered by an administrator only. Ask one to run this from an elevated terminal:\n\n  \
             {} --home {} host {verb} --service",
            this_exe(),
            install::quote_arg(&profile.root().to_string_lossy())
        ),
        Rights::AdminNotElevated => bail!(
            "this needs an elevated terminal: the logon task runs with highest privileges, a service is machine-wide \
             and firewall rules apply to the whole machine.\nOpen one (right-click the terminal, \"Run as \
             administrator\") and run:\n\n  ai-remote --home {} host {verb}{firewall_flag}{service_flag}\n\nAdd \
             --dry-run to see every change first.",
            install::quote_arg(&profile.root().to_string_lossy()),
            service_flag = if service { " --service" } else { "" }
        ),
        Rights::StandardUser if scope == Scope::FirewallOnly => bail!(
            "firewall rules apply to the whole machine and need an administrator. Ask one to run this from an \
             elevated terminal (any administrator account):\n\n  {} host {verb} --firewall-only",
            this_exe()
        ),
        Rights::StandardUser => Ok(false),
    }
}

/// Runs the plan's steps in order; administrator steps only when `with_admin`.
fn execute(plan: &Plan, with_admin: bool) -> Result<()> {
    for (i, action) in plan.actions.iter().enumerate() {
        println!("  {}. {}", i + 1, action.describe().replace('\n', "\n   "));
        if action.needs_admin() && !with_admin {
            println!("     skipped: needs an administrator");
            continue;
        }
        match action.execute() {
            Ok(None) => println!("     ok"),
            Ok(Some(note)) => println!("     skipped: {note}"),
            Err(e) => bail!(
                "step {} failed: {e:#}\nThe steps after it were not run. Fix the cause and run the same command again \
                 (repeating it is safe).",
                i + 1
            ),
        }
    }
    Ok(())
}
