//! `ai-remote host install | uninstall | status` and `ai-remote doctor`: running the host as a
//! background program started at login, and checking that a machine can really be operated.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use ar_core::{Profile, Role};
use ar_host::{
    HostRequest,
    doctor::{self, Grade, Report},
    install::{self, Action, Plan},
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

pub async fn host_install(profile: &Profile, dry_run: bool, no_start: bool) -> Result<()> {
    let p = profile.clone();
    let plan = tokio::task::spawn_blocking(move || install::install_plan(&p)).await??;
    print_plan(&plan, dry_run);
    if dry_run {
        return Ok(());
    }
    require_elevation(&plan, profile, "install")?;
    execute(&plan)?;
    println!("\nAutostart is installed.");
    if no_start {
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

pub async fn host_uninstall(profile: &Profile, dry_run: bool) -> Result<()> {
    let p = profile.clone();
    let plan = tokio::task::spawn_blocking(move || install::uninstall_plan(&p)).await??;
    let running = running_host(profile).await;
    print_plan(&plan, dry_run);
    if let Some(host) = &running {
        println!("  - Also stops the running host (pid {}).", host["pid"]);
    }
    if dry_run {
        return Ok(());
    }
    require_elevation(&plan, profile, "uninstall")?;
    if running.is_some() {
        stop_host(profile).await?;
    }
    execute(&plan)?;
    println!("\nAutostart is removed.");
    Ok(())
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

pub async fn host_status(profile: &Profile, json_out: bool) -> Result<()> {
    let p = profile.clone();
    let status = tokio::task::spawn_blocking(move || install::status(&p)).await?;
    let host = running_host(profile).await;
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
                println!("autostart: {} {} is installed", s.kind, s.name);
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
                let allow = if fw.allow.is_empty() { "no allow rule".to_string() } else { format!("allow {}", fw.allow.join(", ")) };
                let block = if fw.block.is_empty() {
                    String::new()
                } else {
                    format!("; BLOCKED by {}", fw.block.iter().map(|b| format!("\"{b}\"")).collect::<Vec<_>>().join(", "))
                };
                println!("firewall:  {}: {allow}{block}", fw.program);
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
    Ok(())
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

fn print_plan(plan: &Plan, dry_run: bool) {
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
    if plan.needs_elevation {
        println!("\nRun it without --dry-run from an elevated terminal to apply.");
    }
}

fn require_elevation(plan: &Plan, profile: &Profile, verb: &str) -> Result<()> {
    if plan.needs_elevation && ar_host::platform::session_state().elevated == Some(false) {
        bail!(
            "this needs an elevated terminal: the logon task runs with highest privileges and firewall rules apply to \
             the whole machine.\nOpen one (right-click the terminal, \"Run as administrator\") and run:\n\n  ai-remote \
             --home {} host {verb}\n\nAdd --dry-run to see every change first.",
            install::quote_arg(&profile.root().to_string_lossy())
        );
    }
    Ok(())
}

fn execute(plan: &Plan) -> Result<()> {
    for (i, action) in plan.actions.iter().enumerate() {
        println!("  {}. {}", i + 1, action.describe().replace('\n', "\n   "));
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
