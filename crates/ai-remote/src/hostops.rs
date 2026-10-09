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
    let mut conn = Conn::connect(&profile.control_endpoint(Role::Host).ok()?)
        .await
        .ok()?;
    conn.request(&serde_json::to_value(HostRequest::Status).ok()?)
        .await
        .ok()
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
}

pub async fn host_install(profile: &Profile, flags: InstallFlags) -> Result<()> {
    let scope = if flags.firewall_only {
        Scope::FirewallOnly
    } else {
        Scope::All
    };
    let p = profile.clone();
    let plan = tokio::task::spawn_blocking(move || install::install_plan(&p, scope)).await??;
    print_plan(&plan, flags.dry_run);
    if let Some(blocker) = &plan.blocker {
        if !flags.dry_run && !flags.force {
            bail!("{blocker}");
        }
        println!("Warning: {blocker}\n");
    }
    if flags.dry_run {
        return Ok(());
    }
    let with_admin = admin_steps_allowed(&plan, profile, "install", scope)?;
    execute(&plan, with_admin)?;
    if scope == Scope::FirewallOnly {
        println!("\nThe firewall rules are in place.");
        return Ok(());
    }
    println!("\nAutostart is installed.");
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
    println!("\nStarting the background host...");
    let start = install::start_action(profile)?;
    if let Some(note) = start.execute()? {
        println!("  {note}");
    }
    let log = profile.log_path(Role::Host)?;
    let Some(host) = wait_for_host(profile, Duration::from_secs(20)).await else {
        bail!(
            "the background host did not answer within 20 s; see {}",
            log.display()
        );
    };
    println!(
        "The background host is running (pid {}); it logs to {}.\n",
        host["pid"],
        log.display()
    );
    // The self-check: the host's own view of its session, token and network.
    print_report(&host_doctor(profile).await?);
    if host["peers"].as_u64() == Some(0) {
        println!(
            "\nNext: pair a controller. Run `ai-remote pair-code` here and follow its instructions."
        );
    }
    Ok(())
}

pub async fn host_uninstall(profile: &Profile, dry_run: bool, firewall_only: bool) -> Result<()> {
    let scope = if firewall_only {
        Scope::FirewallOnly
    } else {
        Scope::All
    };
    let p = profile.clone();
    let plan = tokio::task::spawn_blocking(move || install::uninstall_plan(&p, scope)).await??;
    let running = if scope == Scope::All {
        running_host(profile).await
    } else {
        None
    };
    print_plan(&plan, dry_run);
    if let Some(host) = &running {
        println!("  - Also stops the running host (pid {}).", host["pid"]);
    }
    if dry_run {
        return Ok(());
    }
    let with_admin = admin_steps_allowed(&plan, profile, "uninstall", scope)?;
    if running.is_some() {
        stop_host(profile).await?;
    }
    execute(&plan, with_admin)?;
    if scope == Scope::FirewallOnly {
        println!("\nDone.");
        return Ok(());
    }
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
        let _ = conn
            .request(&serde_json::to_value(HostRequest::Shutdown)?)
            .await;
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
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"autostart": autostart, "host": host}))?
        );
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
                    println!(
                        "           not this installation or profile: run `ai-remote host install` again"
                    );
                }
            } else {
                let why = s
                    .detail
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
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
                        fw.block
                            .iter()
                            .map(|b| format!("\"{b}\""))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                println!("firewall:  {}: {allow}{block}", fw.program);
            }
            if let Some(fw) = s.firewall.first().filter(|fw| !fw.block_all.is_empty()) {
                println!(
                    "firewall:  inbound UDP of every program is blocked by {} (beats the allow rules; review in wf.msc)",
                    fw.block_all
                        .iter()
                        .map(|b| format!("\"{b}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(e) = &s.firewall_error {
                println!("firewall:  unknown ({e})");
            }
            if s.firewall_enabled == Some(false) {
                println!(
                    "firewall:  Windows Firewall is off on every network profile; the rules matter once it is on"
                );
            }
        }
        Err(e) => println!("autostart: unknown ({e:#})"),
    }
    match host {
        Some(h) => println!(
            "host:      running as \"{}\" (pid {}, {})",
            h["name"].as_str().unwrap_or("?"),
            h["pid"],
            if h["background"].as_bool() == Some(true) {
                "background"
            } else {
                "in a terminal"
            }
        ),
        None => println!("host:      not running"),
    }
    Ok(())
}

/// The running host's doctor report.
async fn host_doctor(profile: &Profile) -> Result<Report> {
    let mut conn = Conn::connect(&profile.control_endpoint(Role::Host)?)
        .await
        .context("connect to the host")?;
    let v = conn
        .request(&serde_json::to_value(HostRequest::Doctor)?)
        .await?;
    serde_json::from_value(v).context("unexpected doctor report from the host")
}

pub async fn doctor(profile: &Profile, json_out: bool) -> Result<()> {
    let report = if running_host(profile).await.is_some() {
        host_doctor(profile).await?
    } else {
        // Same process setup as the host, so the DPI check measures what the host would get.
        ar_host::platform::init_process();
        doctor::run(doctor::Inputs {
            profile,
            mesh: None,
        })
        .await
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
        println!(
            "\nRun it without --dry-run to apply: from an elevated terminal if this account is an administrator. A \
             standard user's run does the rest and leaves the steps marked \"administrator\" to `--firewall-only` \
             in an administrator's elevated terminal."
        );
    }
}

/// Whether this process runs the plan's administrator steps too (`Ok(false)`: a standard user
/// runs the rest). An administrator who is not elevated is sent to an elevated terminal: their
/// logon task needs it for "highest privileges", and so do the firewall rules.
fn admin_steps_allowed(plan: &Plan, profile: &Profile, verb: &str, scope: Scope) -> Result<bool> {
    use ar_host::platform::Rights;
    if !plan.needs_elevation {
        return Ok(true);
    }
    let firewall_flag = if scope == Scope::FirewallOnly {
        " --firewall-only"
    } else {
        ""
    };
    match ar_host::platform::rights() {
        Rights::Elevated => Ok(true),
        Rights::AdminNotElevated => bail!(
            "this needs an elevated terminal: the logon task runs with highest privileges and firewall rules apply to \
             the whole machine.\nOpen one (right-click the terminal, \"Run as administrator\") and run:\n\n  ai-remote \
             --home {} host {verb}{firewall_flag}\n\nAdd --dry-run to see every change first.",
            install::quote_arg(&profile.root().to_string_lossy())
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
