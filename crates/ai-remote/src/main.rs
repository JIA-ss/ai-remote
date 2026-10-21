//! `ai-remote`: run a host, pair with hosts, and bridge AI agents to them over MCP.

mod chat;
mod hostops;
mod remote;

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::{
    Level, Profile, Role,
    config::{self, Action, HostConfig, MountConfig, MountStart, MountTool},
};
use ar_host::{HostRequest, mounts};
use ar_net::ipc::Conn;
use clap::{Parser, Subcommand, ValueEnum};
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
    /// firewall, autostart, ...). Asks the running host; checks from here when none runs. With --host,
    /// asks that paired host instead.
    Doctor {
        /// A paired host to check (from this controller) instead of this machine.
        #[arg(long)]
        host: Option<String>,
    },
    /// [host] Show a one-time pair code (and its QR code) for a new controller, with the ticket
    /// for machines that cannot reach each other over the network.
    PairCode {
        /// Level to grant: observe, control or admin.
        #[arg(long, default_value = "control")]
        level: Level,
        /// This host's name for the controller that pairs with it.
        #[arg(long)]
        name: Option<String>,
    },
    /// [host] List paired controllers and their levels.
    Peers,
    /// [host] Change a controller's level (observe, control, admin or none).
    Level { peer: String, level: String },
    /// [host] Refuse a controller from now on (even with a new ticket) and cut its live sessions.
    Revoke { peer: String },
    /// [host] Lift a revocation (by name, or by device id once the pairing was forgotten).
    Unrevoke { peer: String },
    /// [host] Remove a controller's pairing and cut its live sessions; it can come back only with
    /// a new ticket.
    Forget { peer: String },
    /// [host] Stop the running host.
    StopHost,
    /// [host] Trust paired controllers with everything: their calls (an agent's included) use every
    /// tool here, with no approval and no deny rules, whatever their level. Without a subcommand:
    /// who has it now.
    Bypass {
        #[command(subcommand)]
        action: Option<BypassAction>,
    },
    /// [host] People allowed to approve actions from a paired device: list their approver keys, or
    /// add / remove one.
    Approvers {
        #[command(subcommand)]
        command: Option<ApproversCommand>,
    },
    /// This computer's network settings, as host and controller alike (the desktop app's Settings >
    /// Network): show them, or change how devices connect (`mode auto|lan`), local-network discovery
    /// (`discovery on|off`) or router port mapping (`upnp on|off`).
    Network {
        #[command(subcommand)]
        action: Option<NetworkAction>,
    },
    /// [host] Manage mounted MCP servers (edits the host config; restart the host to apply).
    Mount {
        #[command(subcommand)]
        command: MountCommand,
    },
    /// [controller] Pair with a host using its pair code (`4821 0937`) or its ticket.
    Pair {
        /// The eight-digit pair code the host shows (spaces and dashes are fine), the link in its
        /// QR code, or the whole `mcpmesh-invite:` ticket.
        #[arg(num_args = 1.., value_name = "CODE|TICKET")]
        ticket: Vec<String>,
        /// Your local name for the host.
        #[arg(long)]
        name: Option<String>,
    },
    /// [controller] List paired hosts; `hosts default <name>` sets the host commands and agents use
    /// when they name none.
    Hosts {
        #[command(subcommand)]
        action: Option<HostsAction>,
    },
    /// [controller] The agents (MCP clients) connected to this controller now; --disconnect cuts one
    /// off by its id.
    Agents {
        #[arg(long, value_name = "ID")]
        disconnect: Option<u64>,
    },
    /// [controller] Unpair a host: it removes this device's pairing too (when it can be reached), then
    /// this device forgets it. --local only forgets it here.
    Unpair {
        host: String,
        #[arg(long)]
        local: bool,
    },
    /// [controller] What this device may do on a host: your level, the standing rules that concern
    /// you, the paired devices (all of them for an admin) and the tools on offer.
    Access {
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Lower this device's level on a host (observe, control). Raising it takes someone
    /// at that computer.
    Lower {
        level: Level,
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Follow a host's activity live: every tool call (decision and outcome) and every
    /// session, starting with recent history. --json prints the raw events.
    Watch {
        /// Host name (default: AI_REMOTE_HOST, the default host, or the only paired host).
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Show this device's approver key (created on first use; Windows Hello confirms
    /// it), or offer it to a host with --enroll. With an accepted key you can allow actions from here.
    Approver {
        /// Make the key if this device has none (Windows Hello asks you once). Without it the
        /// command only shows an existing key, so looking never brings up a prompt.
        #[arg(long)]
        create: bool,
        /// Offer the key to this host, making it first if needed; someone there confirms it.
        #[arg(long, value_name = "HOST")]
        enroll: Option<String>,
    },
    /// [controller] List actions waiting for approval on a host.
    Approvals {
        /// Host name (default: AI_REMOTE_HOST, the default host, or the only paired host).
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Allow one waiting action (Windows Hello asks you first).
    Approve {
        /// The approval id from `ai-remote approvals` (or its first 6+ characters).
        id: String,
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Deny one waiting action.
    Deny {
        id: String,
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
        /// Tell the agent why, or what to do instead.
        #[arg(long)]
        note: Option<String>,
    },
    /// [controller] Take the controls of a host: the agent's calls are refused until you hand back.
    TakeOver {
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
        /// Let the agent keep looking at the screen (observe-level tools) meanwhile.
        #[arg(long)]
        let_watch: bool,
    },
    /// [controller] Give the controls back to the agent, optionally telling it what you changed.
    HandBack {
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// [controller] Stop the agent on a host until you resume it.
    Pause {
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Let a paused agent go on.
    Resume {
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Leave a message for the agent working on a host; it arrives with its next call
    /// result.
    Tell {
        text: String,
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
    },
    /// [controller] Watch a host's screen for a while and report the stream (frames, sizes, rate,
    /// codec, arrival gaps); --out saves the raw stream for a player. The desktop app shows the same
    /// stream live.
    View {
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long, default_value_t = 30)]
        max_fps: u32,
        /// Longest edge of the stream in pixels (the host never upscales).
        #[arg(long, default_value_t = 1920)]
        max_long_edge: u32,
        #[arg(long, default_value_t = 6000)]
        bitrate_kbps: u32,
        /// Codecs to offer, best first, comma-separated (h264, hevc, av1); H.264 when omitted.
        #[arg(long, value_delimiter = ',')]
        codecs: Vec<String>,
        /// Save the elementary stream (Annex B, or AV1 OBUs) here.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Ask for the pointer separately (`"cursor":"client"`, as the desktop app does) and count
        /// its position and shape messages.
        #[arg(long)]
        pointer: bool,
    },
    /// [controller] Call one tool on a host and print the result.
    Call {
        /// Tool name, e.g. system_info.
        tool: String,
        /// Tool arguments as a JSON object.
        #[arg(default_value = "{}")]
        args: String,
        /// Host name (default: AI_REMOTE_HOST, the default host, or the only paired host).
        #[arg(long, env = "AI_REMOTE_HOST")]
        host: Option<String>,
        /// You make this call yourself (it runs while you have taken over).
        #[arg(long)]
        person: bool,
    },
    /// [controller] Run a command on a host and wait for it; prints its output and exits with its exit
    /// code (124: timed out; 255: it did not run).
    Exec(remote::ExecArgs),
    /// [controller] Background jobs on a host: start, list, status, log (-f), wait, kill.
    Job {
        #[command(subcommand)]
        command: remote::JobCommand,
    },
    /// [controller] Save a screenshot of a host's monitor.
    Shot(remote::ShotArgs),
    /// [controller] A host's visible windows.
    Windows(remote::WindowsArgs),
    /// [controller] A host's running processes, optionally filtered by name.
    Ps(remote::PsArgs),
    /// [controller] Stop a process on a host (and, by default, the processes it started).
    Kill(remote::KillArgs),
    /// [controller] A host's system, session, monitors and your level there.
    Info {
        #[command(flatten)]
        target: remote::Target,
    },
    /// [controller] List a folder on a host.
    Ls(remote::LsArgs),
    /// [controller] Whether a path exists on a host, and what it is.
    Stat(remote::PathArgs),
    /// [controller] Create a folder on a host.
    Mkdir(remote::MkdirArgs),
    /// [controller] Delete a file or folder on a host.
    Rm(remote::RmArgs),
    /// [controller] Send a file to a host (streamed, SHA-256 checked, moved into place when complete).
    Push(remote::PushArgs),
    /// [controller] Fetch a file from a host (streamed, SHA-256 checked).
    Pull(remote::PullArgs),
    /// [controller] Coding agents on a host (Claude Code, Codex, ...): list what is installed, start
    /// one in a folder and talk to it, or hand it a task. `chat enable|disable|status` [host] allow
    /// or forbid them on this machine.
    Chat {
        #[command(subcommand)]
        command: chat::ChatCommand,
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
enum BypassAction {
    /// Turn it on for a controller (by its name in `ai-remote peers`), or for every paired one.
    On {
        #[arg(required_unless_present = "all")]
        peer: Option<String>,
        #[arg(long, conflicts_with = "peer")]
        all: bool,
        /// End it by itself after this long (90s, 30m, 2h, 1d); default: until you turn it off.
        #[arg(long = "for", value_name = "DURATION")]
        duration: Option<String>,
    },
    /// Turn it off for a controller, or for every one.
    Off {
        #[arg(required_unless_present = "all")]
        peer: Option<String>,
        #[arg(long, conflicts_with = "peer")]
        all: bool,
    },
    /// The default for every paired controller, those paired later included: `on` trusts them all
    /// with everything until you turn one off (`bypass off <controller>`); `off` goes back to
    /// trusting only the controllers you turned on.
    Default { state: OnOff },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum NetworkMode {
    /// Address lookup, hole punching and encrypted relays: connects from anywhere.
    Auto,
    /// Direct on the local network only: no relay, no address lookup; a host listens on UDP 47800.
    Lan,
}

#[derive(Subcommand)]
enum NetworkAction {
    /// How devices connect: `auto` (from anywhere) or `lan` (the local network only).
    Mode { mode: NetworkMode },
    /// Find devices on the local network and be found by them (mDNS, announces this computer about
    /// once a second); off by default.
    Discovery { state: OnOff },
    /// Ask the router to open a port (UPnP, NAT-PMP, PCP); off by default.
    Upnp { state: OnOff },
}

#[derive(Subcommand)]
enum HostsAction {
    /// Show, set (`default <host>`) or clear (`default --clear`) the host commands and agents use when
    /// they name none.
    Default {
        host: Option<String>,
        #[arg(long, conflicts_with = "host")]
        clear: bool,
    },
    /// Show or set the addresses a host is dialled at: `ip`, `ip:port`, several separated by commas
    /// (no port: 47800, a LAN-mode host's default). In LAN mode (`[network] mode = "lan"`) this is
    /// how a host whose address changed is reached again.
    Address { host: String, address: Option<String> },
}

#[derive(Subcommand)]
enum ApproversCommand {
    /// Accept a paired device's approver key without a prompt (the key printed by `ai-remote approver`
    /// on that device). Run it here, at the host.
    Add {
        /// The device's name or id on this host (`ai-remote peers`).
        peer: String,
        /// `<scheme>:<base64 key>` exactly as `ai-remote approver` prints it.
        key: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Remove a device's approver keys (all, or the one with this fingerprint).
    Remove {
        peer: String,
        #[arg(long)]
        fingerprint: Option<String>,
    },
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

#[derive(Subcommand)]
enum HostAction {
    /// Start the host automatically at login, in the background (Windows: a logon task with
    /// highest privileges plus firewall rules; macOS: a LaunchAgent). On Windows an administrator
    /// runs it elevated; a standard user runs it unelevated and leaves the firewall rules to an
    /// administrator (`--firewall-only`).
    Install {
        /// Print every change without making it.
        #[arg(long)]
        dry_run: bool,
        /// Do not start the background host right away.
        #[arg(long)]
        no_start: bool,
        /// Only add the inbound firewall rules for this installation's host programs (Windows,
        /// elevated; they apply to every account).
        #[arg(long, conflicts_with = "no_start")]
        firewall_only: bool,
        /// Install even though another account uses the desktop (the host then starts at this
        /// account's next logon).
        #[arg(long)]
        force: bool,
        /// Windows, elevated: register a service instead of the logon task. It starts the host as
        /// SYSTEM inside the console session, so the lock screen, the sign-in screen and UAC
        /// prompts can be seen and operated; the host's shell, jobs and files tools then run as
        /// SYSTEM too (see --dry-run for the full consequences).
        #[arg(long, conflicts_with = "firewall_only")]
        service: bool,
    },
    /// Start the installed background host now (the autostart entry's own start command), for
    /// example after `stop-host`. The desktop app's tray uses this too.
    Start,
    /// Remove the autostart entry, stop the running host, and remove the firewall rules unless
    /// another profile's autostart still uses them.
    Uninstall {
        /// Print every change without making it.
        #[arg(long)]
        dry_run: bool,
        /// Only remove the firewall rules (Windows, elevated).
        #[arg(long)]
        firewall_only: bool,
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
        Command::Agent => "info,mcpmesh=warn,iroh=warn,noq=warn,rmcp=warn",
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
        Command::Host {
            action: Some(HostAction::Install { dry_run, no_start, firewall_only, force, service }),
            ..
        } => {
            let flags = hostops::InstallFlags { dry_run, no_start, firewall_only, force, service };
            hostops::host_install(&profile, flags).await
        }
        Command::Host { action: Some(HostAction::Uninstall { dry_run, firewall_only }), .. } => {
            hostops::host_uninstall(&profile, dry_run, firewall_only).await
        }
        Command::Host { action: Some(HostAction::Status), .. } => hostops::host_status(&profile, json_out).await,
        Command::Host { action: Some(HostAction::Start), .. } => hostops::host_start(&profile, json_out).await,
        Command::Doctor { host: None } => hostops::doctor(&profile, json_out).await,
        Command::Doctor { host: Some(host) } => remote::doctor(&profile, host, json_out).await,
        Command::PairCode { level, name } => {
            let v = host_request(&profile, HostRequest::PairCode { level, name }).await?;
            if json_out {
                return print_json(&v);
            }
            let line = v["line"].as_str().unwrap_or_default();
            match v["code"]["grouped"].as_str() {
                Some(code) => {
                    if let Some(qr) = v["code"]["link"].as_str().and_then(ar_host::pair_qr::terminal) {
                        for l in qr.lines() {
                            println!("  {l}");
                        }
                        println!();
                    }
                    println!("Scan it with the ai-remote app on a phone, or type the pair code on the other computer:");
                    println!("\n  {code}        (ai-remote pair {})\n", v["code"]["code"].as_str().unwrap_or_default());
                    let until = v["code"]["expires_at"].as_u64().map(ar_host::fmt_epoch).unwrap_or_default();
                    println!("Grants level \"{level}\". Works once; it expires {until}.");
                    println!(
                        "Without a network between the machines, use the ticket instead:\n\n  ai-remote pair {line}\n"
                    );
                }
                None => {
                    match (v["code_error"].as_str(), v["code_reason"].is_string()) {
                        // The error already says what to do instead.
                        (Some(e), true) => eprintln!("note: {e}"),
                        (Some(e), false) => eprintln!("note: no pair code ({e}); use the ticket"),
                        (None, _) => {}
                    }
                    println!("On the controlling machine, run:\n\n  ai-remote pair {line}\n");
                    println!("Grants level \"{level}\". Valid once.");
                }
            }
            println!("Check that both machines show the same three-word safety code.");
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
                    "{:<24} level={:<8} {}{}{}",
                    p["name"].as_str().unwrap_or("?"),
                    p["level"].as_str().unwrap_or("none"),
                    p["principal"].as_str().unwrap_or(""),
                    if p["revoked"].as_bool() == Some(true) { "  REVOKED" } else { "" },
                    if p["bypass"].as_bool() == Some(true) { "  BYPASS" } else { "" }
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
            let mut msg = format!("{} is no longer revoked", v["peer"].as_str().unwrap_or("?"));
            if v["paired"].as_bool() == Some(false) {
                msg.push_str("; it is not paired, so it needs a new ticket (`ai-remote pair-code`) to connect again");
            }
            print_result(json_out, &v, &msg)
        }
        Command::Forget { peer } => {
            let v = host_request(&profile, HostRequest::Forget { peer }).await?;
            let name = v["peer"].as_str().unwrap_or("?");
            let mut msg = format!("forgot {name}; it can come back only with a new ticket");
            if v["still_revoked"].as_bool() == Some(true) {
                msg = format!(
                    "forgot {name}. Its device stays refused because it was revoked; to let it pair again later, run: \
                     ai-remote unrevoke {}",
                    v["device"].as_str().unwrap_or("<device id>")
                );
            }
            print_result(json_out, &v, &msg)
        }
        Command::StopHost => {
            let v = host_request(&profile, HostRequest::Shutdown).await?;
            print_result(json_out, &v, "host is stopping")
        }
        Command::Bypass { action } => bypass_command(&profile, action, json_out).await,
        Command::Mount { command } => mount_command(&profile, command, json_out).await,
        Command::Approvers { command } => approvers_command(&profile, command, json_out).await,
        Command::Approver { create, enroll } => approver_command(&profile, create, enroll, json_out).await,
        Command::TakeOver { host, let_watch } => {
            let request = json!({"turn": {"action": "take_over", "agent_may_watch": let_watch}});
            let v = channel(&profile, host, request).await?;
            let watch = if let_watch { " It may still look at the screen." } else { "" };
            print_result(
                json_out,
                &v,
                &format!(
                    "You have control of {}. The agent waits until you hand back.{watch}",
                    v["host"].as_str().unwrap_or("?")
                ),
            )
        }
        Command::HandBack { host, note } => {
            let v = channel(&profile, host, json!({"turn": {"action": "hand_back", "note": note}})).await?;
            print_result(json_out, &v, "Handed back. The agent reads the screen again before its next step.")
        }
        Command::Pause { host } => {
            let v = channel(&profile, host, json!({"turn": {"action": "pause"}})).await?;
            print_result(json_out, &v, &format!("The agent on {} is paused.", v["host"].as_str().unwrap_or("?")))
        }
        Command::Resume { host } => {
            let v = channel(&profile, host, json!({"turn": {"action": "resume"}})).await?;
            print_result(json_out, &v, "The agent can go on.")
        }
        Command::Tell { text, host } => {
            let v = channel(&profile, host, json!({"tell": {"text": text}})).await?;
            print_result(json_out, &v, "The agent gets your message with its next result.")
        }
        Command::Approvals { host } => {
            let mut agent = ar_controller::connect_agent(&profile).await?;
            let (host, waiting) = ar_controller::approver::pending(&mut agent, host.as_deref()).await?;
            if json_out {
                return print_json(&json!({"host": host, "pending": waiting}));
            }
            if waiting.is_empty() {
                println!("Nothing is waiting for approval on {host}.");
            }
            for w in waiting {
                print_waiting(&w);
            }
            Ok(())
        }
        Command::Approve { id, host } => {
            let mut agent = ar_controller::connect_agent(&profile).await?;
            let (host, waiting) = ar_controller::approver::find(&mut agent, host.as_deref(), &id).await?;
            if !json_out {
                print_waiting(&waiting);
                println!("Confirm with Windows Hello to allow it once...");
            }
            ar_controller::approver::allow(&mut agent, &profile, &host, &waiting).await?;
            print_result(json_out, &json!({"host": host, "id": waiting.id, "allowed": true}), "allowed once")
        }
        Command::Deny { id, host, note } => {
            let mut agent = ar_controller::connect_agent(&profile).await?;
            let (host, waiting) = ar_controller::approver::find(&mut agent, host.as_deref(), &id).await?;
            ar_controller::approver::deny(&mut agent, &host, &waiting.id, note.as_deref()).await?;
            print_result(json_out, &json!({"host": host, "id": waiting.id, "denied": true}), "denied")
        }
        Command::Pair { ticket, name } => {
            // `ai-remote pair 4821 0937` arrives as two words.
            let ticket = ticket.join(" ");
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
        Command::Network { action } => network(&profile, action, json_out).await,
        Command::Hosts { action: None } => {
            let v = agent_request(&profile, json!({"op": "hosts"})).await?;
            if json_out {
                return print_json(&v);
            }
            let hosts = v.as_array().cloned().unwrap_or_default();
            if hosts.is_empty() {
                println!(
                    "No host is paired. On the host run `ai-remote pair-code`, then `ai-remote pair <ticket>` here."
                );
            }
            let mut via_n0 = false;
            for h in hosts {
                via_n0 |= h["relay_n0"] == true;
                println!(
                    "{:<24} level-at-pairing={:<8} {}{}",
                    h["name"].as_str().unwrap_or("?"),
                    h["level_at_pairing"].as_str().unwrap_or("?"),
                    route_text(&h),
                    if h["default"] == true { "  (default)" } else { "" }
                );
            }
            if via_n0 {
                println!(
                    "\nP2P failed for the hosts marked \"n0 public relay\": their traffic is forwarded by n0's free \
                     public relay (still end-to-end encrypted, but higher latency and rate-limited). \
                     To use your own relay see docs/operations.md, section 12 (Connecting across networks)."
                );
            }
            Ok(())
        }
        Command::Hosts { action: Some(HostsAction::Default { host, clear }) } => {
            if host.is_none() && !clear {
                let v = agent_request(&profile, json!({"op": "hosts"})).await?;
                let default =
                    v.as_array().and_then(|a| a.iter().find(|h| h["default"] == true)).map(|h| h["name"].clone());
                let default = default.unwrap_or(Value::Null);
                let text = match default.as_str() {
                    Some(name) => format!("default host: {name}"),
                    None => "no default host: calls that name none go to the only paired host".to_string(),
                };
                return print_result(json_out, &json!({ "default": default }), &text);
            }
            let v = agent_request(&profile, json!({"op": "set_default", "host": host})).await?;
            let text = match v["default"].as_str() {
                Some(name) => format!("default host is now {name}"),
                None => "default host cleared".to_string(),
            };
            print_result(json_out, &v, &text)
        }
        Command::Hosts { action: Some(HostsAction::Address { host, address: None }) } => {
            let v = agent_request(&profile, json!({"op": "hosts"})).await?;
            let found = v.as_array().and_then(|a| a.iter().find(|h| h["name"] == host.as_str())).cloned();
            let Some(h) = found else { bail!("no paired host named '{host}' (`ai-remote hosts` lists them)") };
            let addrs: Vec<&str> = h["addresses"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let text = if addrs.is_empty() {
                format!("{host}: no stored address")
            } else {
                format!("{host} is dialled at {}", addrs.join(", "))
            };
            print_result(json_out, &json!({"host": host, "addresses": h["addresses"]}), &text)
        }
        Command::Hosts { action: Some(HostsAction::Address { host, address: Some(address) }) } => {
            let v =
                agent_request(&profile, json!({"op": "set_host_address", "host": host, "address": address})).await?;
            let addrs: Vec<&str> = v["addresses"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let text = format!("{} is now dialled at {}", v["host"].as_str().unwrap_or(&host), addrs.join(", "));
            print_result(json_out, &v, &text)
        }
        Command::Agents { disconnect: Some(id) } => {
            let v = agent_request(&profile, json!({"op": "disconnect_agent", "id": id})).await?;
            let text =
                if v["disconnected"] == true { format!("agent {id} disconnected") } else { format!("no agent {id}") };
            print_result(json_out, &v, &text)
        }
        Command::Agents { disconnect: None } => {
            let v = agent_request(&profile, json!({"op": "status"})).await?;
            if json_out {
                return print_json(&v["agents"]);
            }
            let agents = v["agents"].as_array().cloned().unwrap_or_default();
            if agents.is_empty() {
                println!("No agent is connected. An MCP client connects with: ai-remote mcp");
            }
            for a in agents {
                println!(
                    "{:<4} {:<24} {} calls",
                    a["id"].as_u64().unwrap_or(0),
                    a["client"].as_str().unwrap_or("(not named yet)"),
                    a["calls"].as_u64().unwrap_or(0)
                );
            }
            Ok(())
        }
        Command::Unpair { host, local } => {
            // Ask the host to drop this device first: after the local unpair it could not be reached.
            let left =
                if local { None } else { Some(channel(&profile, Some(host.clone()), json!({"leave": true})).await) };
            let mut v = agent_request(&profile, json!({"op": "unpair", "host": host})).await?;
            let name = v["host"].as_str().unwrap_or("?").to_string();
            let text = match &left {
                None => format!(
                    "unpaired {name} here; it still lists this device until someone there runs ai-remote forget"
                ),
                Some(Ok(_)) => format!("unpaired {name}; it no longer lists this device"),
                Some(Err(e)) => format!(
                    "unpaired {name} here; it could not be reached ({e:#}), so it still lists this device until someone there runs ai-remote forget"
                ),
            };
            v["host_forgot_this_device"] = matches!(left, Some(Ok(_))).into();
            print_result(json_out, &v, &text)
        }
        Command::Access { host } => {
            let v = channel(&profile, host, json!({"access": true})).await?;
            if json_out {
                return print_json(&v);
            }
            let s = |v: &Value| v.as_str().unwrap_or("-").to_string();
            println!("{}: you are {} with level {}", s(&v["host"]), s(&v["you"]), s(&v["level"]));
            if v["bypass"] == true {
                println!(
                    "bypass: on. The owner trusts this device with everything: every tool, no approvals, no deny rules"
                );
            }
            let rules = v["rules"].as_array().cloned().unwrap_or_default();
            if rules.is_empty() {
                println!("standing rules: none (each tool's default applies)");
            }
            for r in rules {
                println!("rule: {} {} (for {})", s(&r["action"]), s(&r["tool"]), s(&r["scope"]));
            }
            for d in v["devices"].as_array().cloned().unwrap_or_default() {
                let you = if d["you"].as_bool() == Some(true) { " (this device)" } else { "" };
                let revoked = if d["revoked"].as_bool() == Some(true) { ", revoked" } else { "" };
                println!("device: {}{you}: {}{revoked}", s(&d["name"]), s(&d["level"]));
            }
            let tools = |list: &Value| {
                list.as_array()
                    .map(|a| {
                        a.iter()
                            .map(|t| {
                                let asks = if t["action"] == "ask" { ", asks" } else { "" };
                                format!("{} ({}{asks})", s(&t["name"]), s(&t["level"]))
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            };
            println!("built in: {}", tools(&v["builtin"]));
            for m in v["mounts"].as_array().cloned().unwrap_or_default() {
                println!("mount {}: {}", s(&m["name"]), tools(&m["tools"]));
            }
            Ok(())
        }
        Command::Lower { level, host } => {
            let v = channel(&profile, host, json!({"lower": {"level": level}})).await?;
            print_result(json_out, &v, &format!("Your level on {} is {} now.", s_or(&v["host"]), level.as_str()))
        }
        Command::Call { tool, args, host, person } => {
            let args: Value = serde_json::from_str(&args).context("arguments must be a JSON object")?;
            let Value::Object(args) = args else { bail!("arguments must be a JSON object") };
            let v = agent_request(
                &profile,
                json!({"op": "call", "host": host, "tool": tool, "args": args, "person": person}),
            )
            .await?;
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
        Command::Exec(a) => remote::exec(&profile, a, json_out).await,
        Command::Job { command } => remote::job(&profile, command, json_out).await,
        Command::Shot(a) => remote::shot(&profile, a, json_out).await,
        Command::Windows(a) => remote::windows(&profile, a, json_out).await,
        Command::Ps(a) => remote::ps(&profile, a, json_out).await,
        Command::Kill(a) => remote::kill(&profile, a, json_out).await,
        Command::Info { target } => remote::info(&profile, target, json_out).await,
        Command::Ls(a) => remote::ls(&profile, a, json_out).await,
        Command::Stat(a) => remote::stat(&profile, a, json_out).await,
        Command::Mkdir(a) => remote::mkdir(&profile, a, json_out).await,
        Command::Rm(a) => remote::rm(&profile, a, json_out).await,
        Command::Push(a) => remote::push(&profile, a, json_out).await,
        Command::Pull(a) => remote::pull(&profile, a, json_out).await,
        Command::Chat { command } => match command {
            chat::ChatCommand::Enable | chat::ChatCommand::Disable => {
                let on = matches!(command, chat::ChatCommand::Enable);
                let v = host_request(&profile, HostRequest::SetChatEnabled { on }).await?;
                let msg = if on {
                    "coding agents may be started here by devices with level admin (asking first unless bypassed)"
                } else {
                    "coding agents are switched off here; running sessions were ended"
                };
                print_result(json_out, &v, msg)
            }
            chat::ChatCommand::Status => {
                let v = host_request(&profile, HostRequest::Chat).await?;
                let msg = format!(
                    "coding agents: {}; {} session(s), {} running",
                    if v["enabled"] == true { "allowed" } else { "switched off" },
                    v["sessions"].as_array().map_or(0, Vec::len),
                    v["live"].as_u64().unwrap_or(0)
                );
                print_result(json_out, &v, &msg)
            }
            other => chat::command(&profile, other, json_out).await,
        },
        Command::Watch { host } => watch(&profile, host, json_out).await,
        Command::View { host, seconds, max_fps, max_long_edge, bitrate_kbps, codecs, out, pointer } => {
            let mut request = json!({"max_fps": max_fps, "max_long_edge": max_long_edge, "bitrate_kbps": bitrate_kbps,
                "codecs": codecs});
            if pointer {
                request["cursor"] = "client".into();
            }
            view(&profile, host, seconds, request, out, json_out).await
        }
        Command::Mcp => {
            let ended = ar_controller::run_shim(&profile).await;
            // Exit now: the stdin reader is a blocking read the runtime would wait for, so an MCP
            // client whose session ended (disconnected from the app) would only notice at its
            // next request.
            if let Err(e) = &ended {
                eprintln!("Error: {e:#}");
            }
            std::process::exit(if ended.is_ok() { 0 } else { 1 });
        }
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

fn print_waiting(w: &ar_controller::approver::Waiting) {
    let left = (w.expires_ms as i64 - chrono::Utc::now().timestamp_millis()).max(0) / 1000;
    println!("{}  {} wants to run {}  ({left} s left)", &w.id[..12.min(w.id.len())], w.peer, w.tool);
    if let Some(intent) = &w.intent {
        println!("    why: {intent}");
    }
    println!("    {}", w.args_preview);
}

async fn approvers_command(profile: &Profile, command: Option<ApproversCommand>, json_out: bool) -> Result<()> {
    match command {
        None => {
            let v = host_request(profile, HostRequest::Approvers).await?;
            if json_out {
                return print_json(&v);
            }
            let keys = v["approvers"].as_array().cloned().unwrap_or_default();
            if keys.is_empty() {
                println!(
                    "No approver keys: only someone at this computer can approve. On a paired device, run \
                     `ai-remote approver --enroll <this host>` to add one."
                );
            }
            for k in keys {
                let label = k["label"].as_str().filter(|l| !l.is_empty()).unwrap_or("(added at the host)");
                println!(
                    "{:<24} {}  {label}",
                    k["device"].as_str().unwrap_or("?"),
                    k["fingerprint"].as_str().unwrap_or("?")
                );
            }
            if v["approval_prompt"] == false {
                println!("Prompts on this computer are off (approval_prompt = false).");
            }
            Ok(())
        }
        Some(ApproversCommand::Add { peer, key, label }) => {
            let (alg, key) = key.split_once(':').ok_or_else(|| anyhow!("expected <scheme>:<base64 key>"))?;
            let alg: ar_core::approval::KeyAlg =
                serde_json::from_value(json!(alg)).map_err(|_| anyhow!("unknown key scheme '{alg}'"))?;
            let v = host_request(profile, HostRequest::AddApprover { peer, alg, key: key.to_string(), label }).await?;
            let msg = if v["added"] == true {
                format!(
                    "{} can now approve from that device (key {})",
                    v["peer"].as_str().unwrap_or("?"),
                    v["fingerprint"].as_str().unwrap_or("?")
                )
            } else {
                "that key was already accepted".to_string()
            };
            print_result(json_out, &v, &msg)
        }
        Some(ApproversCommand::Remove { peer, fingerprint }) => {
            let v = host_request(profile, HostRequest::RemoveApprover { peer, fingerprint }).await?;
            print_result(json_out, &v, &format!("removed {} approver key(s)", v["removed"]))
        }
    }
}

async fn approver_command(profile: &Profile, create: bool, enroll: Option<String>, json_out: bool) -> Result<()> {
    let p = profile.clone();
    let make = create || enroll.is_some();
    let key = tokio::task::spawn_blocking(move || {
        if make { ar_controller::approver::get_or_create(&p).map(Some) } else { ar_controller::approver::existing(&p) }
    })
    .await??;
    let Some(key) = key else {
        if json_out {
            return print_json(&json!({"key": null}));
        }
        println!("This device has no approver key yet. Make one with:\n\n  ai-remote approver --create\n");
        println!("Windows Hello (Touch ID on a Mac) asks you once; denying from here works without a key.");
        return Ok(());
    };
    let shown = json!({
        "label": key.label,
        "fingerprint": key.fingerprint(),
        "key": format!("{}:{}", key.alg.as_str(), key.public_key_b64()),
    });
    let Some(host) = enroll else {
        if json_out {
            return print_json(&shown);
        }
        println!("Approver key: {} ({})", key.label, key.fingerprint());
        println!("\nTo accept it on a host, either run here:\n\n  ai-remote approver --enroll <host>\n");
        println!(
            "or, at the host:\n\n  ai-remote approvers add <this device's name there> {}",
            shown["key"].as_str().unwrap_or("")
        );
        return Ok(());
    };
    if !json_out {
        println!("Asking someone at {host} to accept approver key {} ...", key.fingerprint());
    }
    let mut agent = ar_controller::connect_agent(profile).await?;
    let v = ar_controller::approver::enroll(&mut agent, profile, Some(&host)).await?;
    let msg = if v["added"] == true {
        format!("{host} accepted this device's approver key. You can now allow its actions from here.")
    } else {
        format!("{host} had already accepted this key.")
    };
    print_result(json_out, &v, &msg)
}

/// `ai-remote bypass`: who the owner trusts with everything here, and turning that on or off.
async fn bypass_command(profile: &Profile, action: Option<BypassAction>, json_out: bool) -> Result<()> {
    let list = |v: &Value| v["devices"].as_array().cloned().unwrap_or_default();
    let (on, peer, duration) = match action {
        None => {
            let v = host_request(profile, HostRequest::Bypass).await?;
            if json_out {
                return print_json(&v);
            }
            let devices = list(&v);
            if v["default"] == true {
                println!(
                    "default: on. Every paired controller, including ones paired later, is trusted with everything \
                     unless turned off below (ai-remote bypass off <controller>)."
                );
            }
            if devices.is_empty() {
                println!("No controller is paired.");
            }
            for d in devices {
                let state = match (d["how"].as_str(), d["until_ms"].as_u64()) {
                    (Some("device"), Some(until)) => format!("BYPASS until {}", local_time(until)),
                    (Some("device"), None) => "BYPASS".to_string(),
                    (Some("default"), _) => "BYPASS (default)".to_string(),
                    (Some("excepted"), _) => "- (turned off; the default does not cover it)".to_string(),
                    _ => "-".to_string(),
                };
                println!("{:<24} level={:<8} {state}", s_or(&d["name"]), d["level"].as_str().unwrap_or("none"));
            }
            return Ok(());
        }
        Some(BypassAction::Default { state }) => {
            let on = state == OnOff::On;
            let v = host_request(profile, HostRequest::SetBypassDefault { on }).await?;
            if json_out {
                return print_json(&v);
            }
            match (on, v["changed"] == true) {
                (true, true) => println!(
                    "bypass default on. Every paired controller, including ones you pair later, now uses every tool \
                     here (shell, files, input, ...) without approval, whatever its level or the deny rules. Turn one \
                     off with: ai-remote bypass off <controller>; turn the default off with: ai-remote bypass default off"
                ),
                (true, false) => println!("bypass default was already on"),
                (false, true) => println!("bypass default off. Only controllers turned on one by one stay bypassed."),
                (false, false) => println!("bypass default was already off"),
            }
            return Ok(());
        }
        Some(BypassAction::On { peer, duration, .. }) => (true, peer, duration),
        Some(BypassAction::Off { peer, .. }) => (false, peer, None),
    };
    let for_secs = duration.as_deref().map(parse_duration).transpose()?;
    let peers: Vec<String> = match peer {
        Some(p) => vec![p],
        // --all: on for every device that can connect at all; off for every one that has it.
        None => list(&host_request(profile, HostRequest::Bypass).await?)
            .iter()
            .filter(|d| if on { d["level"].is_string() && d["revoked"] != true } else { d["how"] == "device" })
            .filter_map(|d| d["name"].as_str().map(str::to_string))
            .collect(),
    };
    let mut results = Vec::new();
    for peer in peers {
        let v = host_request(profile, HostRequest::SetBypass { peer, on, for_secs }).await?;
        if !json_out {
            let name = s_or(&v["peer"]);
            match (on, v["until_ms"].as_u64()) {
                (true, Some(until)) => println!("bypass on for {name} until {}", local_time(until)),
                (true, None) => println!("bypass on for {name} until you turn it off"),
                (false, _) if v["changed"] == true => println!("bypass off for {name}"),
                (false, _) => println!("{name} had no bypass"),
            }
        }
        results.push(v);
    }
    if json_out {
        return print_json(&Value::Array(results));
    }
    if results.is_empty() {
        println!(
            "{}",
            if on { "No controller can connect here, so nothing changed." } else { "No controller had bypass." }
        );
    } else if on {
        println!(
            "Their calls now use every tool here (shell, files, input, ...) without approval, whatever their level or \
             the deny rules. Turn it off with: ai-remote bypass off <controller> (or --all)"
        );
    }
    Ok(())
}

/// `90s`, `30m`, `2h`, `1d` or plain seconds.
fn parse_duration(text: &str) -> Result<u64> {
    let t = text.trim();
    let (number, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: u64 = number.parse().map_err(|_| anyhow!("'{text}' is not a duration (e.g. 90s, 30m, 2h, 1d)"))?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("'{text}' is not a duration (e.g. 90s, 30m, 2h, 1d)"),
    };
    if secs == 0 {
        bail!("give a duration longer than zero");
    }
    Ok(secs)
}

fn local_time(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

fn s_or(v: &Value) -> &str {
    v.as_str().unwrap_or("?")
}

async fn channel(profile: &Profile, host: Option<String>, request: Value) -> Result<Value> {
    let mut agent = ar_controller::connect_agent(profile).await?;
    ar_controller::approver::channel(&mut agent, host.as_deref(), request).await
}

/// Reads a host's live view for a while: counts frames, checks their framing, optionally saves them.
async fn view(
    profile: &Profile,
    host: Option<String>,
    seconds: u64,
    request: Value,
    out: Option<PathBuf>,
    json_out: bool,
) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut conn = ar_controller::connect_agent(profile).await?;
    let reply = conn.request(&json!({"op": "screen", "host": host, "request": request})).await?;
    let host = reply["host"].as_str().unwrap_or("?").to_string();
    let (stream, buffered) = conn.into_raw();
    let (read, mut write) = tokio::io::split(stream);
    let mut stream = std::io::Cursor::new(buffered).chain(read);
    // Pings map the host's stream clock onto ours (the reply with the shortest round trip wins), so
    // each frame's age since capture can be told. Hosts from before 2026-10-12 do not answer.
    let pings: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u64, std::time::Instant>>> =
        Default::default();
    let pinger = {
        let pings = pings.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            for n in 0u64.. {
                pings.lock().unwrap().insert(n, std::time::Instant::now());
                if write.write_all(format!("{{\"ping\":{n}}}\n").as_bytes()).await.is_err() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(if n < 10 { 100 } else { 500 })).await;
            }
        })
    };
    // (round trip, the local instant of the host clock's zero) of the best pong so far.
    let mut clock: Option<(std::time::Duration, std::time::Instant)> = None;
    let mut arrivals: Vec<(u64, std::time::Instant)> = Vec::new();
    let mut file = match &out {
        Some(p) => Some(std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?),
        None => None,
    };
    let (mut frames, mut keyframes, mut bytes, mut size) = (0u64, 0u64, 0u64, (0u16, 0u16));
    let (mut codec, mut gaps, mut last, mut first_ms) = (0u8, Vec::new(), None, None);
    let separate_pointer = reply["cursor"] == "client";
    // Positions and shapes received, and their bytes.
    let (mut pointer_messages, mut pointer_bytes) = ([0u64; 2], 0u64);
    let started = std::time::Instant::now();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        let mut header = [0u8; 18];
        match tokio::time::timeout_at(deadline, stream.read_exact(&mut header)).await {
            Err(_) => break,
            Ok(Err(e)) => bail!("the live view ended: {e}"),
            Ok(Ok(_)) => {}
        }
        let len = u32::from_be_bytes(header[14..18].try_into()?) as usize;
        let ts = u64::from_be_bytes(header[6..14].try_into()?);
        if header[0] == 3 && len <= 4096 {
            let now = std::time::Instant::now();
            let mut payload = vec![0u8; len];
            stream.read_exact(&mut payload).await?;
            let n = serde_json::from_slice::<Value>(&payload)?["pong"].as_u64();
            if let Some(sent) = n.and_then(|n| pings.lock().unwrap().remove(&n)) {
                let rtt = now.duration_since(sent);
                // The host stamped the pong halfway through the round trip, as best we know.
                let zero = (sent + rtt / 2).checked_sub(std::time::Duration::from_micros(ts));
                if let Some(zero) = zero
                    && clock.is_none_or(|(best, _)| rtt < best)
                {
                    clock = Some((rtt, zero));
                }
            }
            continue;
        }
        if matches!(header[0], 4 | 5) {
            // The pointer, asked for with --pointer: counted, not drawn.
            let mut payload = vec![0u8; len];
            stream.read_exact(&mut payload).await?;
            pointer_messages[usize::from(header[0] - 4)] += 1;
            pointer_bytes += 18 + len as u64;
            continue;
        }
        if header[0] != 1 {
            bail!("unexpected frame kind {}", header[0]);
        }
        let mut data = vec![0u8; len];
        stream.read_exact(&mut data).await?;
        arrivals.push((ts, std::time::Instant::now()));
        frames += 1;
        keyframes += (header[1] & 1) as u64;
        bytes += len as u64;
        codec = (header[1] >> 1) & 3;
        let now = std::time::Instant::now();
        first_ms.get_or_insert(now.duration_since(started).as_secs_f64() * 1000.0);
        if let Some(previous) = last.replace(now) {
            gaps.push(now.duration_since(previous).as_secs_f64() * 1000.0);
        }
        size = (u16::from_be_bytes([header[2], header[3]]), u16::from_be_bytes([header[4], header[5]]));
        if let Some(f) = file.as_mut() {
            std::io::Write::write_all(f, &data)?;
        }
    }
    let secs = started.elapsed().as_secs_f64();
    pinger.abort();
    // Capture -> received, for every frame, with the final (best) clock mapping.
    let mut latency: Vec<f64> = match clock {
        Some((_, zero)) => arrivals
            .iter()
            .map(|(ts, at)| {
                let captured = zero + std::time::Duration::from_micros(*ts);
                at.saturating_duration_since(captured).as_secs_f64() * 1000.0
                    - captured.saturating_duration_since(*at).as_secs_f64() * 1000.0
            })
            .collect(),
        None => Vec::new(),
    };
    latency.sort_by(f64::total_cmp);
    let lat = |p: f64| latency.get(((latency.len().max(1) - 1) as f64 * p) as usize).copied();
    let rtt_ms = clock.map(|(rtt, _)| rtt.as_secs_f64() * 1000.0);
    let codec = ["h264", "hevc", "av1"].get(codec as usize).copied().unwrap_or("unknown");
    gaps.sort_by(f64::total_cmp);
    let gap = |p: f64| gaps.get(((gaps.len().max(1) - 1) as f64 * p) as usize).copied();
    let v = json!({
        "host": host,
        "monitors": reply["monitors"],
        "codec": codec,
        "frames": frames,
        "keyframes": keyframes,
        "width": size.0,
        "height": size.1,
        "fps": frames as f64 / secs,
        "kbit_per_s": bytes as f64 * 8.0 / secs / 1000.0,
        "first_frame_ms": first_ms,
        "gap_ms": {"p50": gap(0.5), "p95": gap(0.95), "max": gap(1.0)},
        // Capture on the host to arrival here; error up to half the round trip either way.
        "latency_ms": {"p50": lat(0.5), "p95": lat(0.95), "max": lat(1.0), "error": rtt_ms.map(|r| r / 2.0)},
        "rtt_ms": rtt_ms,
        "pointer": {"separate": separate_pointer, "positions": pointer_messages[0], "shapes": pointer_messages[1],
            "bytes": pointer_bytes},
    });
    if json_out {
        return print_json(&v);
    }
    println!(
        "{host}: {frames} {codec} frames ({keyframes} keyframes) at {}x{} in {secs:.1} s, {:.1} fps, {:.0} kbit/s; first after {:.0} ms, gaps p50 {:.1} / p95 {:.1} / max {:.1} ms",
        size.0,
        size.1,
        frames as f64 / secs,
        bytes as f64 * 8.0 / secs / 1000.0,
        first_ms.unwrap_or(0.0),
        gap(0.5).unwrap_or(0.0),
        gap(0.95).unwrap_or(0.0),
        gap(1.0).unwrap_or(0.0),
    );
    match (lat(0.5), lat(0.95), rtt_ms) {
        (Some(p50), Some(p95), Some(rtt)) => println!(
            "Capture to arrival: p50 {p50:.1} ms, p95 {p95:.1} ms (within +/- {:.1} ms; round trip {rtt:.1} ms)",
            rtt / 2.0
        ),
        _ => println!("Capture to arrival: unknown (the host does not answer pings; an older version?)"),
    }
    if separate_pointer {
        println!(
            "Pointer sent separately: {} positions, {} shapes, {pointer_bytes} bytes",
            pointer_messages[0], pointer_messages[1]
        );
    }
    println!("Frames arrive only when the screen changes; a still screen sends almost nothing.");
    if let Some(p) = out {
        println!("Saved to {} ({codec} elementary stream; e.g. ffplay <file>)", p.display());
    }
    Ok(())
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
        Some(h) => println!(
            "host:    running as \"{}\" ({}), {} paired controller(s)",
            h["name"].as_str().unwrap_or("?"),
            h["device"].as_str().unwrap_or("?"),
            h["peers"]
        ),
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

/// How a host's open connection travels, for `ai-remote hosts`.
fn route_text(h: &Value) -> String {
    let relay = h["relay"].as_str().map(|r| r.split_once("://").map_or(r, |(_, host)| host).trim_end_matches('.'));
    let why = match h["p2p_obstacle"].as_str() {
        Some("udp_blocked") => " (this network blocks UDP)",
        Some("symmetric_nat") => " (this network's NAT is symmetric)",
        Some("captive_portal") => " (this network wants a sign-in page first)",
        Some("firewall") => " (this computer's firewall blocks incoming connections for ai-remote)",
        Some(_) => " (likely the host's network)",
        None => "",
    };
    match (h["path"].as_str(), relay) {
        (Some("direct"), _) => "direct (P2P)".into(),
        (Some("relay"), Some(relay)) if h["relay_n0"] == true => format!("n0 public relay ({relay}): P2P failed{why}"),
        (Some("relay"), Some(relay)) => format!("relay {relay}: P2P failed{why}"),
        (Some("relay"), None) => format!("relay: P2P failed{why}"),
        (Some(_), _) => "route unknown".into(),
        (None, _) => "not connected".into(),
    }
}

/// `ai-remote network`: shows or changes this computer's network settings in both the host's and
/// the controller's `ai-remote.toml` (what the desktop app's Settings > Network does). A change
/// restarts the controller agent so it applies now; a running host applies it at its next start.
async fn network(profile: &Profile, action: Option<NetworkAction>, json_out: bool) -> Result<()> {
    let paths = [profile.host_config_path()?, profile.controller_config_path()?];
    let changed = action.is_some();
    if matches!(action, Some(NetworkAction::Discovery { state: OnOff::On } | NetworkAction::Upnp { state: OnOff::On }))
    {
        config::ensure_not_quiet(&[&paths[0], &paths[1]])?;
    }
    if let Some(action) = action {
        for path in &paths {
            match action {
                NetworkAction::Mode { mode } => {
                    config::set_network_mode(path, if mode == NetworkMode::Lan { "lan" } else { "auto" })?
                }
                NetworkAction::Discovery { state } => {
                    config::set_lan_discovery(path, if state == OnOff::On { "on" } else { "off" })?
                }
                NetworkAction::Upnp { state } => config::set_port_mapping(path, state == OnOff::On)?,
            }
        }
    }
    let host = ar_net::NetworkOptions::from_config(&HostConfig::load(&paths[0])?.network, Role::Host)?;
    let controller =
        ar_net::NetworkOptions::from_config(&config::ControllerConfig::load(&paths[1])?.network, Role::Controller)?;
    let agent_restarted = changed && ar_controller::restart_agent(profile).await;
    let v = json!({
        "mode": if host.lan_direct || controller.lan_direct { "lan" } else { "auto" },
        "lan_discovery": host.lan != ar_net::LanDiscovery::Off || controller.lan != ar_net::LanDiscovery::Off,
        "port_mapping": host.port_mapping || controller.port_mapping,
        "locked": host.quiet || controller.quiet,
        "host_port": host.port,
        "agent_restarted": agent_restarted,
    });
    let on = |b: bool| if b { "on" } else { "off" };
    let mut text = format!(
        "connect: {}\nlocal-network discovery: {}\nrouter port mapping (UPnP): {}",
        if v["mode"] == "lan" {
            "local network only (no relay, no address lookup)"
        } else {
            "automatic (address lookup, hole punching, encrypted relays)"
        },
        on(v["lan_discovery"] == true),
        on(v["port_mapping"] == true),
    );
    if host.quiet || controller.quiet {
        text.push_str("\nquiet = true in ai-remote.toml keeps local-network discovery and port mapping off");
    }
    if let (true, Some(port)) = (host.lan_direct, host.port) {
        text.push_str(&format!("\nas a host this computer listens on UDP port {port}"));
    }
    if changed {
        text.push_str("\nsaved; the controller uses it now, a host running here when it next starts");
    }
    print_result(json_out, &v, &text)
}

fn print_result(json_out: bool, v: &Value, human: &str) -> Result<()> {
    if json_out {
        print_json(v)
    } else {
        println!("{human}");
        Ok(())
    }
}

/// Prints a host's activity feed until the host ends it or Ctrl+C.
async fn watch(profile: &Profile, host: Option<String>, json_out: bool) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut conn = ar_controller::connect_agent(profile).await?;
    let v = conn.request(&json!({"op": "events", "host": host})).await?;
    if !json_out {
        eprintln!("Watching {}. Press Ctrl+C to stop.", v["host"].as_str().unwrap_or("the host"));
    }
    let (stream, buffered) = conn.into_raw();
    let mut lines = BufReader::new(std::io::Cursor::new(buffered).chain(stream)).lines();
    while let Some(line) = lines.next_line().await? {
        if json_out {
            println!("{line}");
        } else if let Some(text) = serde_json::from_str::<Value>(&line).ok().as_ref().and_then(describe_event) {
            println!("{text}");
        }
    }
    Ok(())
}

/// One activity event as a line for people; `None` for events not worth a line.
fn describe_event(v: &Value) -> Option<String> {
    let time = v["ts"]
        .as_u64()
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".into());
    let peer = v["peer"].as_str().unwrap_or("?");
    let tool = v["tool"].as_str().unwrap_or("?");
    let text = match v["kind"].as_str()? {
        "session_start" => format!("{peer} connected"),
        "session_end" => format!("{peer} disconnected"),
        "decision" => {
            let decision = v["decision"].as_str().unwrap_or("?");
            match v["intent"].as_str() {
                Some(intent) => format!("{peer}  {tool}  {decision}  \"{intent}\""),
                None => format!("{peer}  {tool}  {decision}"),
            }
        }
        "outcome" => {
            let outcome = v["outcome"].as_str().unwrap_or("?");
            let ms = v["duration_ms"].as_u64().map(|d| format!("  {d} ms")).unwrap_or_default();
            let error = v["error"].as_str().map(|e| format!("  {e}")).unwrap_or_default();
            format!("{peer}  {tool}  {outcome}{ms}{error}")
        }
        "lagged" => format!("(missed {} events: this terminal fell behind)", v["missed"]),
        _ => return None,
    };
    Some(format!("{time}  {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("45").unwrap(), 45);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("1d").unwrap(), 86_400);
        for bad in ["", "h", "2x", "0m", "-1h", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

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
