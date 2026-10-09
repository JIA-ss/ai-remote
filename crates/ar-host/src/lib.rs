//! The ai-remote host: serves this machine's capabilities as MCP tools to paired controllers, behind
//! per-device permission levels, owner approval and an audit log.
//!
//! The host must run inside the interactive desktop session of the logged-in user (never as a
//! service): capture, input injection and approval prompts all need that desktop.

mod approval;
mod audit;
mod control;
mod gate;
pub mod mounts;
pub mod platform;
mod policy;
mod protocol;
pub mod result_budget;
mod server;
pub mod session;
pub mod tools;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use ar_core::{Level, Profile, Role, config::HostConfig, identity, machine_name};
use ar_net::{ALPN_MCP, Mesh, MeshOptions, Peer, ipc};
use tokio_util::sync::CancellationToken;

use crate::{approval::Approver, audit::AuditLog, tools::Registry};

pub use control::HostRequest;

/// Everything a running host shares between sessions.
pub struct HostState {
    profile: Profile,
    name: String,
    config: HostConfig,
    mesh: Mesh,
    registry: Registry,
    audit: AuditLog,
    approver: Approver,
    jobs: tools::jobs::JobManager,
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

    fn audit(&self) -> &AuditLog {
        &self.audit
    }

    fn approver(&self) -> &Approver {
        &self.approver
    }

    /// Approval prompts on screen whose answer is awaited; input tools stay off while any is open.
    pub(crate) fn open_prompts(&self) -> approval::OpenPrompts {
        self.approver.open_prompts()
    }

    /// The level this host grants `peer` right now (`None` when unpaired or no level).
    pub async fn current_level(&self, peer: &Peer) -> Option<Level> {
        let peers = self.mesh.peers().await.ok()?;
        peers
            .into_iter()
            .find(|p| p.principal.is_some() && p.principal == peer.principal)
            .and_then(|p| p.level)
    }
}

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct HostOptions {
    /// Print a pairing ticket at start when no controller is paired yet.
    pub pair_code_if_unpaired: bool,
}

/// Runs the host until Ctrl+C or a `shutdown` request on the control channel.
pub async fn run(profile: Profile, opts: HostOptions) -> Result<()> {
    platform::init_process();
    let config_path = profile.host_config_path()?;
    let config = HostConfig::load(&config_path)?;
    let name = config.host.name.clone().unwrap_or_else(machine_name);

    let endpoint = profile.control_endpoint(Role::Host)?;
    let mut listener = ipc::Listener::bind(&endpoint).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!("a host is already running for this profile ({})", profile.root().display())
        } else {
            anyhow!("bind control endpoint {}: {e}", endpoint.display())
        }
    })?;

    let device_key = identity::load_or_create(&profile.identity_path(Role::Host)?)?;
    let mesh = Mesh::start(MeshOptions {
        root: profile.mesh_dir(Role::Host)?,
        device_key,
        nickname: name.clone(),
        relays: config.network.relays.clone(),
    })
    .await?;
    mesh.ensure_level_services().await?;

    let mut registry = Registry::builtin();
    mounts::extend(&mut registry, &config, &profile).await;

    let state = Arc::new(HostState {
        audit: AuditLog::open(&profile.audit_log_path()?)?,
        approver: Approver::new(Duration::from_secs(config.host.approval_timeout_secs.max(5))),
        jobs: tools::jobs::JobManager::new(profile.jobs_dir()?),
        profile,
        name,
        config,
        mesh: mesh.clone(),
        registry,
    });
    mesh.accept(ALPN_MCP, Arc::new(protocol::McpProtocol::new(state.clone())))?;

    let peers = mesh.peers().await.context("read paired devices")?;
    println!("ai-remote host \"{}\" is running (device {}).", state.name, mesh.short_id());
    println!("  paired controllers: {}", peers.len());
    println!("  audit log: {}", state.audit.path().display());
    if let Some(reason) = platform::prompt_blocker() {
        println!("  warning: approval prompts cannot be shown ({reason}); actions that need approval will be refused");
    }
    if peers.is_empty() && opts.pair_code_if_unpaired {
        let invite = mesh.invite(Level::Control, None).await?;
        println!();
        println!("No controller is paired yet. On the controlling machine, run:");
        println!();
        println!("  ai-remote pair {}", invite.line);
        println!();
        println!("It grants level \"control\" and is valid once, until {}.", fmt_epoch(invite.expires_at));
        println!("Check that both machines show the same three-word safety code.");
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
    println!("ai-remote host stopped.");
    Ok(())
}

fn fmt_epoch(epoch: u64) -> String {
    let remaining = epoch.saturating_sub(ar_core::unix_now());
    format!("{} minutes from now", remaining / 60)
}
