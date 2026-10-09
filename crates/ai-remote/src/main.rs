//! `ai-remote`: run a host, pair with hosts, and bridge AI agents to them over MCP.

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::{
    Level, Profile, Role,
    config::{self, Action, HostConfig, MountConfig, MountStart, MountTool},
};
use ar_host::{HostRequest, mounts};
use ar_net::ipc::Conn;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "ai-remote", version, about = "Remote control where AI agents are first-class operators.")]
struct Cli {
    /// Profile directory (default: the platform's local data directory; env AI_REMOTE_HOME).
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the host on this machine (in the foreground) so paired controllers can operate it.
    Host {
        /// Do not print a pairing ticket when no controller is paired yet.
        #[arg(long)]
        no_pair_code: bool,
    },
    /// [host] Print a one-time pairing ticket for a new controller.
    PairCode {
        /// Level to grant: observe, control or admin.
        #[arg(long, default_value = "control")]
        level: Level,
        /// This host's name for the controller that redeems it.
        #[arg(long)]
        name: Option<String>,
    },
    /// [host] List paired controllers and their levels.
    Peers,
    /// [host] Change a controller's level (observe, control, admin or none).
    Level { peer: String, level: String },
    /// [host] Refuse a controller from now on and cut its live sessions.
    Revoke { peer: String },
    /// [host] Lift a revocation.
    Unrevoke { peer: String },
    /// [host] Remove a controller's pairing entirely.
    Forget { peer: String },
    /// [host] Stop the running host.
    StopHost,
    /// [host] Manage mounted MCP servers (edits the host config; restart the host to apply).
    Mount {
        #[command(subcommand)]
        command: MountCommand,
    },
    /// [controller] Pair with a host using its ticket.
    Pair {
        ticket: String,
        /// Your local name for the host.
        #[arg(long)]
        name: Option<String>,
    },
    /// [controller] List paired hosts.
    Hosts,
    /// [controller] Forget a host.
    Unpair { host: String },
    /// [controller] Call one tool on a host and print the result.
    Call {
        /// Tool name, e.g. system_info.
        tool: String,
        /// Tool arguments as a JSON object.
        #[arg(default_value = "{}")]
        args: String,
        /// Host name (optional when only one host is paired).
        #[arg(long)]
        host: Option<String>,
    },
    /// [controller] stdio MCP server for AI agents (e.g. `claude mcp add ai-remote -- ai-remote mcp`).
    Mcp,
    /// [controller] Run the controller agent in the foreground (normally started automatically).
    Agent,
    /// [controller] Stop the controller agent.
    StopAgent,
    /// Show this machine's device ids and whether the host and agent are running.
    Status,
}

#[derive(Subcommand)]
enum MountCommand {
    /// List the configured mounts and their allowlisted tools.
    List,
    /// Mount a local MCP server (stdio). Only the tools you allowlist are exposed, as <name>_<tool>.
    Add {
        /// Mount name, the prefix of its tool names (`ui` gives ui_click).
        name: String,
        /// Path of the server executable. Its SHA-256 is pinned and checked before every start.
        #[arg(long)]
        command: String,
        /// Argument passed to the server (repeat for several).
        #[arg(long = "arg", value_name = "ARG", allow_hyphen_values = true)]
        args: Vec<String>,
        /// Environment variable for the server (repeat for several).
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Allowlist a tool at a level, optionally with an approval rule: click=control,
        /// launch_app=control:ask (repeat). Tools not listed stay hidden.
        #[arg(long = "tool", value_name = "NAME=LEVEL[:ask|allow|deny]")]
        tools: Vec<String>,
        /// Start the server on first use (and stop it when idle) instead of with the host.
        #[arg(long)]
        lazy: bool,
        /// Refuse its tools while the desktop is locked (UI automation servers).
        #[arg(long)]
        needs_desktop: bool,
        /// Do not pin the executable's hash (allows a program name looked up on PATH, e.g. npx.cmd).
        #[arg(long)]
        no_pin: bool,
    },
    /// Mount a server with a curated profile shipped with ai-remote (allowlist, levels, settings).
    AddProfile {
        /// Profile id, e.g. cua-driver.
        profile: String,
        /// Path of the installed server executable (ai-remote does not download it).
        #[arg(long)]
        command: String,
        /// Mount name (default: the profile's, e.g. `ui`).
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a mount.
    Remove { name: String },
    /// Start a mount's server once and list every tool it offers, marking the allowlisted ones.
    Test { name: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(home) = &cli.home {
        // SAFETY: set before any thread is started.
        unsafe { std::env::set_var(ar_core::profile::HOME_ENV, home) };
    }
    init_logging(&cli.command);
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run(cli))
}

fn init_logging(command: &Command) {
    // stdout belongs to the MCP protocol in `mcp`; logs always go to stderr.
    let default = match command {
        Command::Host { .. } | Command::Agent => "info,mcpmesh=warn,iroh=warn,noq=warn,rmcp=warn",
        _ => "warn",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("AI_REMOTE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}

async fn run(cli: Cli) -> Result<()> {
    let profile = Profile::open_default()?;
    let json_out = cli.json;
    match cli.command {
        Command::Host { no_pair_code } => {
            ar_host::run(profile, ar_host::HostOptions { pair_code_if_unpaired: !no_pair_code }).await
        }
        Command::PairCode { level, name } => {
            let v = host_request(&profile, HostRequest::PairCode { level, name }).await?;
            if json_out {
                return print_json(&v);
            }
            let line = v["line"].as_str().unwrap_or_default();
            println!("On the controlling machine, run:\n\n  ai-remote pair {line}\n");
            println!("Grants level \"{level}\". Valid once. Check that both machines show the same safety code.");
            Ok(())
        }
        Command::Peers => {
            let v = host_request(&profile, HostRequest::Peers).await?;
            if json_out {
                return print_json(&v);
            }
            let peers = v.as_array().cloned().unwrap_or_default();
            if peers.is_empty() {
                println!("No controller is paired. Run `ai-remote pair-code` to pair one.");
            }
            for p in peers {
                println!(
                    "{:<24} level={:<8} {}",
                    p["name"].as_str().unwrap_or("?"),
                    p["level"].as_str().unwrap_or("none"),
                    p["principal"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        Command::Level { peer, level } => {
            let level = match level.as_str() {
                "none" => None,
                other => Some(other.parse::<Level>().map_err(anyhow::Error::msg)?),
            };
            let v = host_request(&profile, HostRequest::SetLevel { peer, level }).await?;
            print_result(json_out, &v, &format!("{} now has level {}", v["peer"].as_str().unwrap_or("?"), v["level"]))
        }
        Command::Revoke { peer } => {
            let v = host_request(&profile, HostRequest::Revoke { peer }).await?;
            print_result(json_out, &v, &format!("revoked {}", v["peer"].as_str().unwrap_or("?")))
        }
        Command::Unrevoke { peer } => {
            let v = host_request(&profile, HostRequest::Unrevoke { peer }).await?;
            print_result(json_out, &v, &format!("{} is no longer revoked", v["peer"].as_str().unwrap_or("?")))
        }
        Command::Forget { peer } => {
            let v = host_request(&profile, HostRequest::Forget { peer }).await?;
            print_result(json_out, &v, &format!("forgot {}", v["peer"].as_str().unwrap_or("?")))
        }
        Command::StopHost => {
            let v = host_request(&profile, HostRequest::Shutdown).await?;
            print_result(json_out, &v, "host is stopping")
        }
        Command::Mount { command } => mount_command(&profile, command, json_out).await,
        Command::Pair { ticket, name } => {
            let v = agent_request(&profile, json!({"op": "pair", "ticket": ticket, "name": name})).await?;
            if json_out {
                return print_json(&v);
            }
            println!("Paired with host \"{}\".", v["peer"].as_str().unwrap_or("?"));
            println!("Safety code: {}  (the host must show the same words)", v["sas"].as_str().unwrap_or("?"));
            if let Some(level) = v["level"].as_str() {
                println!("Granted level: {level}");
            }
            Ok(())
        }
        Command::Hosts => {
            let v = agent_request(&profile, json!({"op": "hosts"})).await?;
            if json_out {
                return print_json(&v);
            }
            let hosts = v.as_array().cloned().unwrap_or_default();
            if hosts.is_empty() {
                println!("No host is paired. On the host run `ai-remote pair-code`, then `ai-remote pair <ticket>` here.");
            }
            for h in hosts {
                println!(
                    "{:<24} level-at-pairing={}",
                    h["name"].as_str().unwrap_or("?"),
                    h["level_at_pairing"].as_str().unwrap_or("?")
                );
            }
            Ok(())
        }
        Command::Unpair { host } => {
            let v = agent_request(&profile, json!({"op": "unpair", "host": host})).await?;
            print_result(json_out, &v, &format!("unpaired {}", v["host"].as_str().unwrap_or("?")))
        }
        Command::Call { tool, args, host } => {
            let args: Value = serde_json::from_str(&args).context("arguments must be a JSON object")?;
            let Value::Object(args) = args else { bail!("arguments must be a JSON object") };
            let v = agent_request(&profile, json!({"op": "call", "host": host, "tool": tool, "args": args})).await?;
            if json_out {
                return print_json(&v);
            }
            let is_error = v["isError"].as_bool().unwrap_or(false);
            for block in v["content"].as_array().cloned().unwrap_or_default() {
                match block["type"].as_str() {
                    Some("text") => println!("{}", block["text"].as_str().unwrap_or_default()),
                    Some("image") => println!(
                        "[image {} , {} base64 chars]",
                        block["mimeType"].as_str().unwrap_or("?"),
                        block["data"].as_str().map(str::len).unwrap_or(0)
                    ),
                    _ => println!("{block}"),
                }
            }
            if is_error {
                std::process::exit(2);
            }
            Ok(())
        }
        Command::Mcp => ar_controller::run_shim(&profile).await,
        Command::Agent => ar_controller::run_agent(profile).await,
        Command::StopAgent => {
            let endpoint = profile.control_endpoint(Role::Controller)?;
            match Conn::connect(&endpoint).await {
                Ok(mut c) => {
                    c.request(&json!({"op": "shutdown"})).await?;
                    println!("agent is stopping");
                }
                Err(_) => println!("agent is not running"),
            }
            Ok(())
        }
        Command::Status => status(&profile, json_out).await,
    }
}

async fn host_request(profile: &Profile, req: HostRequest) -> Result<Value> {
    let endpoint = profile.control_endpoint(Role::Host)?;
    let mut conn = Conn::connect(&endpoint)
        .await
        .map_err(|_| anyhow::anyhow!("the host is not running for this profile; start it with `ai-remote host`"))?;
    conn.request(&serde_json::to_value(req)?).await
}

async fn agent_request(profile: &Profile, req: Value) -> Result<Value> {
    let mut conn = ar_controller::connect_agent(profile).await?;
    conn.request(&req).await
}

async fn status(profile: &Profile, json_out: bool) -> Result<()> {
    let host = match Conn::connect(&profile.control_endpoint(Role::Host)?).await {
        Ok(mut c) => c.request(&json!({"op": "status"})).await.ok(),
        Err(_) => None,
    };
    let agent = match Conn::connect(&profile.control_endpoint(Role::Controller)?).await {
        Ok(mut c) => c.request(&json!({"op": "status"})).await.ok(),
        Err(_) => None,
    };
    let v = json!({"profile": profile.root(), "host": host, "agent": agent});
    if json_out {
        return print_json(&v);
    }
    println!("profile: {}", profile.root().display());
    match host {
        Some(h) => println!("host:    running as \"{}\" ({}), {} paired controller(s)", h["name"].as_str().unwrap_or("?"), h["device"].as_str().unwrap_or("?"), h["peers"]),
        None => println!("host:    not running"),
    }
    match agent {
        Some(a) => println!("agent:   running ({}), hosts: {}", a["device"].as_str().unwrap_or("?"), a["hosts"]),
        None => println!("agent:   not running (starts automatically when needed)"),
    }
    Ok(())
}

async fn mount_command(profile: &Profile, command: MountCommand, json_out: bool) -> Result<()> {
    let path = dunce::simplified(&profile.host_config_path()?).to_path_buf();
    match command {
        MountCommand::List => {
            let cfg = HostConfig::load(&path)?;
            if json_out {
                return print_json(&serde_json::to_value(&cfg.mounts)?);
            }
            if cfg.mounts.is_empty() {
                println!("No mounted servers. Add one with `ai-remote mount add-profile cua-driver --command <path>`");
                println!("or `ai-remote mount add <name> --command <path> --tool <name>=<level>`.");
            }
            let clashes = mounts::name_clashes(&cfg.mounts);
            for m in &cfg.mounts {
                println!("{}", m.name);
                println!(
                    "  command: {}{}",
                    m.command,
                    if m.args.is_empty() { String::new() } else { format!(" {}", m.args.join(" ")) }
                );
                let pin = if m.sha256.is_some() { "sha256 pinned" } else { "not pinned" };
                let start = if m.start == MountStart::Lazy { "lazy start" } else { "starts with the host" };
                println!("  {pin}, {start}{}", if m.needs_desktop { ", needs the desktop" } else { "" });
                if m.tools.is_empty() {
                    println!("  no tools allowlisted: it exposes nothing (see `ai-remote mount test {}`)", m.name);
                }
                for (tool, entry) in &m.tools {
                    let clash = clashes.iter().find(|c| c.mount == m.name && &c.tool == tool);
                    let note = clash.map(|c| format!("  (not exposed: the name is taken by {})", c.taken_by));
                    println!(
                        "  {:<32} {}{}",
                        mounts::tool_name(&m.name, tool),
                        describe_entry(entry),
                        note.unwrap_or_default()
                    );
                }
            }
            println!("\nconfig: {}", path.display());
            Ok(())
        }
        MountCommand::Add { name, command, args, env, tools, lazy, needs_desktop, no_pin } => {
            let (command, sha256) = pin_command(&command, no_pin)?;
            let env = env
                .iter()
                .map(|kv| {
                    kv.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .ok_or_else(|| anyhow!("--env '{kv}': expected KEY=VALUE"))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            let tools = tools.iter().map(|t| parse_tool_entry(t)).collect::<Result<BTreeMap<_, _>>>()?;
            let mount = MountConfig {
                name,
                command,
                args,
                env,
                sha256,
                start: if lazy { MountStart::Lazy } else { MountStart::Eager },
                needs_desktop,
                tools,
            };
            refuse_clashes(&path, &mount, "pick another mount name")?;
            config::add_mount(&path, &mount)?;
            if json_out {
                return print_json(&serde_json::to_value(&mount)?);
            }
            println!("Added mount '{}' ({}).", mount.name, mount.command);
            if mount.tools.is_empty() {
                println!(
                    "No tools are allowlisted yet, so it exposes nothing. `ai-remote mount test {}` lists",
                    mount.name
                );
                println!("the server's tools; add them under [mount.tools] in {}.", path.display());
            }
            restart_hint(profile).await;
            Ok(())
        }
        MountCommand::AddProfile { profile: id, command, name } => {
            let curated = mounts::profiles::get(&id)?;
            let (about, checked) = (curated.about.clone(), curated.checked_version.clone());
            let (command, sha256) = pin_command(&command, false)?;
            let mount = curated.into_config(name, command, sha256);
            refuse_clashes(&path, &mount, "pick another mount name with --name")?;
            config::add_mount(&path, &mount)?;
            if json_out {
                return print_json(&serde_json::to_value(&mount)?);
            }
            println!("Added mount '{}' from profile '{id}' ({about}).", mount.name);
            println!("{} tools allowlisted; the profile was checked against version {checked}.", mount.tools.len());
            println!(
                "Run `ai-remote mount test {}` to compare it with the tools of the installed version.",
                mount.name
            );
            restart_hint(profile).await;
            Ok(())
        }
        MountCommand::Remove { name } => {
            if !config::remove_mount(&path, &name)? {
                bail!("no mount named '{name}' in {} (see `ai-remote mount list`)", path.display());
            }
            if json_out {
                return print_json(&json!({"removed": name}));
            }
            println!("Removed mount '{name}'.");
            restart_hint(profile).await;
            Ok(())
        }
        MountCommand::Test { name } => {
            let cfg = HostConfig::load(&path)?;
            let Some(mc) = cfg.mounts.iter().find(|m| m.name == name) else {
                bail!("no mount named '{name}' in {} (see `ai-remote mount list`)", path.display());
            };
            let mount = mounts::Mount::new(mc.clone(), &profile.mounts_dir()?);
            eprintln!("starting {} ...", mount.command().display());
            let offered = mount.list_tools().await;
            mount.stop().await;
            let mut offered = offered?;
            offered.sort_by(|a, b| a.name.cmp(&b.name));
            let plan = mounts::plan(mc, &offered);
            if json_out {
                let tools: Vec<Value> = offered
                    .iter()
                    .map(|t| {
                        let exposed = plan.tools.iter().find(|p| p.upstream == t.name.as_ref());
                        json!({
                            "name": t.name,
                            "exposed_as": exposed.map(|p| p.spec.name.clone()),
                            "level": exposed.map(|p| p.spec.level),
                            "default_action": exposed.map(|p| p.spec.default_action),
                            "description": t.description,
                        })
                    })
                    .collect();
                return print_json(&json!({"mount": name, "tools": tools, "allowlisted_but_missing": plan.missing}));
            }
            println!(
                "{} offers {} tools; {} are exposed, {} stay hidden:",
                name,
                offered.len(),
                plan.tools.len(),
                plan.hidden.len()
            );
            for t in &offered {
                let status = match plan.tools.iter().find(|p| p.upstream == t.name.as_ref()) {
                    Some(p) => format!("-> {} ({})", p.spec.name, describe_action(p.spec.level, p.spec.default_action)),
                    None => "hidden".to_string(),
                };
                let summary = t.description.as_deref().unwrap_or("").lines().next().unwrap_or("");
                println!("  {:<28} {:<44} {}", t.name, status, clip(summary, 60));
            }
            for missing in &plan.missing {
                println!("  {missing:<28} allowlisted, but the server does not offer it");
            }
            Ok(())
        }
    }
}

/// Refuses a new mount whose tool names are taken by built-in tools or by mounts already configured
/// (the host would not expose them).
fn refuse_clashes(path: &std::path::Path, mount: &MountConfig, hint: &str) -> Result<()> {
    let mut all = HostConfig::load(path)?.mounts;
    all.retain(|m| m.name != mount.name); // a duplicate name is reported by add_mount
    all.push(mount.clone());
    let clashes: Vec<String> = mounts::name_clashes(&all)
        .into_iter()
        .filter(|c| c.mount == mount.name)
        .map(|c| format!("{} (tool '{}') is taken by {}", c.name, c.tool, c.taken_by))
        .collect();
    if !clashes.is_empty() {
        bail!(
            "mount '{}' cannot expose these names, so the host would hide them: {}; {hint}, or leave the tool out",
            mount.name,
            clashes.join("; ")
        );
    }
    Ok(())
}

/// Absolute path and SHA-256 of a mount's executable (or the command as given with `no_pin`).
fn pin_command(command: &str, no_pin: bool) -> Result<(String, Option<String>)> {
    let given = std::path::Path::new(command);
    let absolute = std::path::absolute(given).ok().filter(|p| p.is_file());
    if no_pin {
        // A bare program name stays as it is and is looked up on PATH when the server starts.
        let bare = given.components().count() <= 1 && !given.is_absolute();
        return Ok((
            if bare { command.to_string() } else { absolute.unwrap_or(given.into()).display().to_string() },
            None,
        ));
    }
    let path = absolute.ok_or_else(|| {
        anyhow!(
            "cannot find the file '{command}': give the path of the server executable \
             (or --no-pin to run a program looked up on PATH)"
        )
    })?;
    let sha = mounts::sha256_file(&path)?;
    Ok((path.display().to_string(), Some(sha)))
}

/// `click=control` or `launch_app=control:ask`.
fn parse_tool_entry(spec: &str) -> Result<(String, MountTool)> {
    let usage = || anyhow!("--tool '{spec}': expected NAME=LEVEL or NAME=LEVEL:ask|allow|deny");
    let (name, rest) = spec.split_once('=').ok_or_else(usage)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(usage());
    }
    let (level, approve) = match rest.split_once(':') {
        Some((level, approve)) => (level, Some(approve)),
        None => (rest, None),
    };
    let level: Level = level.parse().map_err(|e: String| anyhow!("--tool '{spec}': {e}"))?;
    let entry = match approve.map(|a| a.trim().to_ascii_lowercase()) {
        None => MountTool::Level(level),
        Some(a) => {
            let approve = match a.as_str() {
                "ask" => Action::Ask,
                "allow" => Action::Allow,
                "deny" => Action::Deny,
                _ => return Err(usage()),
            };
            MountTool::Detailed { level, approve: Some(approve), description: None }
        }
    };
    Ok((name.to_string(), entry))
}

fn describe_entry(entry: &MountTool) -> String {
    describe_action(entry.level(), entry.action())
}

fn describe_action(level: Level, action: Action) -> String {
    match action {
        Action::Allow => level.to_string(),
        Action::Ask => format!("{level}, asks the owner"),
        Action::Deny => format!("{level}, denied"),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}...", s.chars().take(max - 3).collect::<String>()) }
}

/// Mount changes are read when the host starts.
async fn restart_hint(profile: &Profile) {
    let running = match profile.control_endpoint(Role::Host) {
        Ok(endpoint) => Conn::connect(&endpoint).await.is_ok(),
        Err(_) => false,
    };
    if running {
        println!("The host is running: restart it to apply (ai-remote stop-host, then ai-remote host).");
    } else {
        println!("Takes effect the next time the host starts.");
    }
}

fn print_json(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn print_result(json_out: bool, v: &Value, human: &str) -> Result<()> {
    if json_out { print_json(v) } else {
        println!("{human}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_entries() {
        let (name, entry) = parse_tool_entry("click=control").unwrap();
        assert_eq!(name, "click");
        assert_eq!(entry.level(), Level::Control);
        assert_eq!(entry.approve(), None);
        let (_, entry) = parse_tool_entry("launch_app=control:ask").unwrap();
        assert_eq!(entry.approve(), Some(Action::Ask));
        let (_, entry) = parse_tool_entry("kill_app=admin:DENY").unwrap();
        assert_eq!((entry.level(), entry.approve()), (Level::Admin, Some(Action::Deny)));
        for bad in ["click", "=control", "click=root", "click=control:maybe"] {
            assert!(parse_tool_entry(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pinning_needs_a_real_file() {
        assert!(pin_command("definitely-not-here.exe", false).is_err());
        let (cmd, sha) = pin_command("npx.cmd", true).unwrap();
        assert_eq!((cmd.as_str(), sha), ("npx.cmd", None));
        let exe = std::env::current_exe().unwrap();
        let (cmd, sha) = pin_command(&exe.display().to_string(), false).unwrap();
        assert_eq!(std::path::PathBuf::from(cmd), exe);
        assert_eq!(sha.unwrap().len(), 64);
    }
}
