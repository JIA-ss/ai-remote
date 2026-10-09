//! `ai-remote`: run a host, pair with hosts, and bridge AI agents to them over MCP.

mod hostops;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use ar_core::{Level, Profile, Role};
use ar_host::HostRequest;
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
    /// Run the host on this machine (in the foreground) so paired controllers can operate it;
    /// `host install` starts it automatically at login instead.
    #[command(args_conflicts_with_subcommands = true)]
    Host {
        #[command(subcommand)]
        action: Option<HostAction>,
        /// Do not print a pairing ticket when no controller is paired yet.
        #[arg(long)]
        no_pair_code: bool,
    },
    /// Check whether this machine can be operated as a host (session, desktop, elevation, network,
    /// firewall, autostart, ...). Asks the running host; checks from here when none runs.
    Doctor,
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
enum HostAction {
    /// Start the host automatically at login, in the background (Windows: a logon task with
    /// highest privileges plus firewall rules, from an elevated terminal; macOS: a LaunchAgent).
    Install {
        /// Print every change without making it.
        #[arg(long)]
        dry_run: bool,
        /// Do not start the background host right away.
        #[arg(long)]
        no_start: bool,
    },
    /// Remove the autostart entry and firewall rules, and stop the running host.
    Uninstall {
        /// Print every change without making it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show whether autostart is installed, the firewall rules, and whether the host runs.
    Status,
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
        Command::Host { action: None, .. } => ar_host::DEFAULT_LOG_FILTER,
        Command::Agent => "info,mcpmesh=warn,iroh=warn,noq=warn",
        // The firewall reader warns about every rule it cannot model.
        _ => "warn,windows_firewall=error",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("AI_REMOTE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}

async fn run(cli: Cli) -> Result<()> {
    let profile = Profile::open_default()?;
    let json_out = cli.json;
    match cli.command {
        Command::Host { action: None, no_pair_code } => {
            let opts = ar_host::HostOptions { pair_code_if_unpaired: !no_pair_code, background: false };
            ar_host::run(profile, opts).await
        }
        Command::Host { action: Some(HostAction::Install { dry_run, no_start }), .. } => {
            hostops::host_install(&profile, dry_run, no_start).await
        }
        Command::Host { action: Some(HostAction::Uninstall { dry_run }), .. } => {
            hostops::host_uninstall(&profile, dry_run).await
        }
        Command::Host { action: Some(HostAction::Status), .. } => hostops::host_status(&profile, json_out).await,
        Command::Doctor => hostops::doctor(&profile, json_out).await,
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
