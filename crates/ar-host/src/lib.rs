//! The ai-remote host: serves this machine's capabilities as MCP tools to paired controllers, behind
//! per-device permission levels, owner approval and an audit log.
//!
//! The host must run inside the interactive desktop session: capture, input injection and approval
//! prompts all need that desktop. It runs either in a terminal (`ai-remote host`), in the
//! background as the logged-in user, started at login by the entry `ai-remote host install`
//! registers (see [`install`]), or on Windows as SYSTEM inside the console session, started by
//! the service `ai-remote host install --service` registers (see [`service`]), which is what
//! reaches the lock screen, the sign-in screen and UAC prompts.

mod approval;
mod approvers;
mod audit;
pub mod bypass;
pub mod chat;
mod chat_channel;
mod control;
pub mod doctor;
mod events;
mod files_channel;
mod gate;
pub mod install;
pub mod mounts;
pub mod platform;
mod policy;
mod protocol;
pub mod result_budget;
mod server;
#[cfg(windows)]
pub mod service;
pub mod session;
pub mod stream;
pub mod supervise;
pub mod tools;
mod turns;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use ar_core::{Level, Profile, Role, config::HostConfig, identity, machine_name};
use ar_net::{
    ALPN_CHAT, ALPN_EVENTS, ALPN_FILES, ALPN_MCP, ALPN_SCREEN, LanDiscovery, Mesh, MeshOptions, NetworkOptions, Peer,
    ipc,
};
use tokio_util::sync::CancellationToken;

use crate::{approval::Approver, audit::AuditLog, tools::Registry};

pub use control::HostRequest;

/// Log filter for a running host when `AI_REMOTE_LOG` is not set: our own events, only problems
/// from the transport and the MCP library (which logs every session's full peer info at info).
/// The firewall reader warns about every rule it cannot model; not useful here.
pub const DEFAULT_LOG_FILTER: &str = "info,mcpmesh=warn,iroh=warn,noq=warn,rmcp=warn,windows_firewall=error";

/// Everything a running host shares between sessions.
pub struct HostState {
    profile: Profile,
    name: String,
    config: HostConfig,
    mesh: Mesh,
    registry: Registry,
    audit: AuditLog,
    approver: Approver,
    turns: turns::Turns,
    jobs: tools::jobs::JobManager,
    /// Approved file transfers waiting for their bytes.
    transfers: tools::files::Transfers,
    /// Controllers the owner trusts with everything (see [`bypass`]).
    bypass: bypass::Bypass,
    /// Coding agents started and chatted with from controllers (see [`chat`]).
    chat: chat::ChatManager,
    background: bool,
    /// Cancelled when the host stops.
    stopping: CancellationToken,
    live: protocol::LiveConns,
}

impl HostState {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    pub fn mesh(&self) -> &Mesh {
        &self.mesh
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn jobs(&self) -> &tools::jobs::JobManager {
        &self.jobs
    }

    pub fn transfers(&self) -> &tools::files::Transfers {
        &self.transfers
    }

    pub fn bypass(&self) -> &bypass::Bypass {
        &self.bypass
    }

    pub fn chat(&self) -> &chat::ChatManager {
        &self.chat
    }

    /// Runs without a console (started at login); messages go to the host log only.
    pub fn background(&self) -> bool {
        self.background
    }

    /// Cancelled when the host is stopping.
    pub fn stopping(&self) -> &CancellationToken {
        &self.stopping
    }

    /// Controller connections being served.
    pub(crate) fn live(&self) -> &protocol::LiveConns {
        &self.live
    }

    pub(crate) fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// Who drives this computer right now: the agent, a person who took over, or nobody (paused).
    pub(crate) fn turns(&self) -> &turns::Turns {
        &self.turns
    }

    pub(crate) fn approver(&self) -> &Approver {
        &self.approver
    }

    /// Approval prompts on screen whose answer is awaited; input tools stay off while any is open.
    pub(crate) fn open_prompts(&self) -> approval::OpenPrompts {
        self.approver.open_prompts()
    }

    /// The level this host grants `peer` right now (`None` when unpaired or no level).
    pub async fn current_level(&self, peer: &Peer) -> Option<Level> {
        let peers = self.mesh.peers().await.ok()?;
        peers.into_iter().find(|p| p.principal.is_some() && p.principal == peer.principal).and_then(|p| p.level)
    }

    /// The level `peer`'s calls are checked against: admin while the owner bypasses it (see
    /// [`bypass`]) and it still has a level here, its own level otherwise.
    pub async fn effective_level(&self, peer: &Peer) -> Option<Level> {
        let level = self.current_level(peer).await?;
        Some(if self.bypass.covers(peer) { Level::Admin } else { level })
    }
}

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct HostOptions {
    /// Print a pairing ticket at start when no controller is paired yet.
    pub pair_code_if_unpaired: bool,
    /// No console: everything meant for the owner goes to the log. A pairing ticket is a bearer
    /// credential, so a background host never mints one by itself.
    pub background: bool,
}

/// Where messages for the host owner go: the terminal in the foreground, the log otherwise.
#[derive(Clone, Copy)]
struct Owner {
    background: bool,
}

impl Owner {
    fn say(self, line: &str) {
        if self.background {
            if !line.trim().is_empty() {
                tracing::info!("{}", line.trim());
            }
        } else {
            println!("{line}");
        }
    }
}

/// Another host already runs for the profile: not a failure worth retrying.
#[derive(Debug)]
pub struct AlreadyRunning(pub std::path::PathBuf);

impl std::fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a host is already running for this profile ({})", self.0.display())
    }
}

impl std::error::Error for AlreadyRunning {}

/// This profile's host slot (its control endpoint). Holding it proves that no other host runs for
/// the profile, so a second start (a double-click, a script) is turned away before it touches
/// anything the running host owns, its log included. Needs a tokio runtime context.
pub struct Claim {
    listener: ipc::Listener,
}

impl Claim {
    /// Fails with [`AlreadyRunning`] when a host already holds the slot.
    pub fn acquire(profile: &Profile) -> Result<Self> {
        Self::acquire_shared(profile, &[])
    }

    /// [`acquire`](Self::acquire) whose control endpoint also admits the accounts in `shared_with`
    /// (Windows SIDs): the host the service runs as SYSTEM lists the account that installed it.
    pub fn acquire_shared(profile: &Profile, shared_with: &[String]) -> Result<Self> {
        let endpoint = profile.control_endpoint(Role::Host)?;
        let listener = ipc::Listener::bind_shared(&endpoint, shared_with).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                anyhow::Error::new(AlreadyRunning(profile.root().to_path_buf()))
            } else {
                anyhow!("bind control endpoint {}: {e}", endpoint.display())
            }
        })?;
        Ok(Self { listener })
    }
}

/// Runs the host until Ctrl+C or a `shutdown` request on the control channel.
pub async fn run(profile: Profile, opts: HostOptions) -> Result<()> {
    let claim = Claim::acquire(&profile)?;
    run_claimed(profile, claim, opts).await
}

/// [`run`] with the profile's slot already claimed.
pub async fn run_claimed(profile: Profile, claim: Claim, opts: HostOptions) -> Result<()> {
    platform::init_process();
    let owner = Owner { background: opts.background };
    let config_path = profile.host_config_path()?;
    let config = HostConfig::load(&config_path)?;
    let name = config.host.name.clone().unwrap_or_else(machine_name);
    let mut listener = claim.listener;

    let device_key = identity::load_or_create(&profile.identity_path(Role::Host)?)?;
    let mesh = Mesh::start(MeshOptions {
        root: profile.mesh_dir(Role::Host)?,
        device_key,
        nickname: name.clone(),
        network: NetworkOptions::from_config(&config.network, LanDiscovery::On)?,
    })
    .await?;
    mesh.ensure_level_services().await?;

    let mut registry = Registry::builtin();
    mounts::extend(&mut registry, &config, &profile).await;
    let (chat, mut chat_audit) = chat::ChatManager::new(profile.chat_dir()?, profile.chat_config_path()?);

    let state = Arc::new(HostState {
        audit: AuditLog::open(&profile.audit_log_path()?)?,
        approver: Approver::new(
            Duration::from_secs(config.host.approval_timeout_secs.max(5)),
            config.host.approval_prompt,
            approvers::ApproverKeys::open(&profile.approvers_path()?)?,
        ),
        turns: turns::Turns::default(),
        jobs: tools::jobs::JobManager::new(profile.jobs_dir()?),
        transfers: tools::files::Transfers::default(),
        bypass: bypass::Bypass::open(&profile.bypass_path()?)?,
        chat,
        profile,
        name,
        config,
        mesh: mesh.clone(),
        registry,
        background: opts.background,
        stopping: CancellationToken::new(),
        live: protocol::LiveConns::default(),
    });
    mesh.accept(ALPN_MCP, Arc::new(protocol::McpProtocol::new(state.clone())))?;
    mesh.accept(ALPN_EVENTS, Arc::new(events::EventsProtocol::new(state.clone())))?;
    mesh.accept(ALPN_SCREEN, Arc::new(stream::protocol::ScreenProtocol::new(state.clone())))?;
    mesh.accept(ALPN_FILES, Arc::new(files_channel::FilesProtocol::new(state.clone())))?;
    mesh.accept(ALPN_CHAT, Arc::new(chat_channel::ChatProtocol::new(state.clone())))?;
    {
        let state = state.clone();
        tokio::spawn(async move {
            while let Some(v) = chat_audit.recv().await {
                state.audit().event(v);
            }
        });
    }
    tokio::spawn(chat_channel::enforce(Arc::downgrade(&state)));
    // Before any ticket is shown: a pairing that completes before the watch starts is not reported.
    announce_pairings(&mesh, owner).await;

    let peers = mesh.peers().await.context("read paired devices")?;
    owner.say(&format!("ai-remote host \"{}\" is running (device {}).", state.name, mesh.short_id()));
    owner.say(&format!("  profile: {}", state.profile.root().display()));
    if platform::runs_as_system() {
        owner.say("  runs as SYSTEM (service mode): the lock screen and UAC prompts can be seen and operated; shell, jobs and files run as SYSTEM");
    }
    owner.say(&format!("  paired controllers: {}", peers.len()));
    owner.say(&format!("  audit log: {}", state.audit.path().display()));
    let approvers = state.approver.keys().list().len();
    if approvers > 0 {
        owner.say(&format!("  approver keys: {approvers}"));
    }
    if !state.config.host.approval_prompt {
        if approvers == 0 {
            owner.say(
                "  warning: approval_prompt = false and no approver keys; actions that need approval will be refused",
            );
        }
    } else if let Some(reason) = platform::prompt_blocker() {
        let then = if approvers > 0 { "only approver devices can answer" } else { "they will be refused" };
        owner.say(&format!("  warning: approval prompts cannot be shown ({reason}); {then}"));
    }
    if peers.is_empty() && opts.pair_code_if_unpaired && !opts.background {
        let invite = mesh.invite(Level::Control, None).await?;
        owner.say("");
        owner.say("No controller is paired yet. On the controlling machine, run:");
        owner.say("");
        owner.say(&format!("  ai-remote pair {}", invite.line));
        owner.say("");
        owner.say(&format!("It grants level \"control\" and is valid once, until {}.", fmt_epoch(invite.expires_at)));
        owner.say("Check that both machines show the same three-word safety code.");
    }

    let shutdown = CancellationToken::new();
    loop {
        tokio::select! {
            conn = listener.accept() => match conn {
                Ok(conn) => {
                    let state = state.clone();
                    let shutdown = shutdown.clone();
                    tokio::spawn(async move { control::serve(state, conn, shutdown).await });
                }
                Err(e) => tracing::warn!(%e, "control connection failed"),
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = shutdown.cancelled() => break,
        }
    }
    // Close live controller connections and give the close frames a moment to leave.
    state.stopping.cancel();
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.say("ai-remote host stopped.");
    Ok(())
}

/// Shows the host side of every pairing ceremony: the controller printed a three-word safety code
/// when it redeemed the ticket, and the host owner must see the same words here. Different words
/// mean someone else redeemed the ticket.
async fn announce_pairings(mesh: &Mesh, owner: Owner) {
    let mut watch = match mesh.watch_pairings().await {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("cannot watch for new pairings ({e:#}); safety codes will not be shown here");
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            match watch.next().await {
                Ok(Some(p)) => {
                    let level = p.level.map(|l| l.to_string()).unwrap_or_else(|| "none".into());
                    tracing::info!(peer = %p.peer, %level, sas = %p.sas, principal = ?p.principal, "controller paired");
                    if !owner.background {
                        println!();
                        println!("Paired with controller \"{}\" (level: {level}).", p.peer);
                        println!("  Safety code: {}", p.sas);
                        println!(
                            "  The controller must show the same words. If it does not, run: ai-remote forget {}",
                            p.peer
                        );
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("stopped watching for new pairings: {e:#}");
                    break;
                }
            }
        }
    });
}

fn fmt_epoch(epoch: u64) -> String {
    let remaining = epoch.saturating_sub(ar_core::unix_now());
    format!("{} minutes from now", remaining / 60)
}
