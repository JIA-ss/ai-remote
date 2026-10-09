//! The controller agent: one per user and profile, started on demand by the CLI or the MCP shim.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::{Profile, Role, VERSION, identity, machine_name, unix_now};
use ar_net::{ALPN_MCP, Mesh, MeshOptions, PathKind, iroh::endpoint::Connection, ipc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    aggregator::Aggregator,
    hosts::{KnownHost, KnownHosts},
};

/// The agent exits after this long without any client.
const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentRequest {
    Status,
    /// Redeem a host's pairing ticket.
    Pair { ticket: String, name: Option<String> },
    Hosts,
    /// Forget a host (its pairing on this controller).
    Unpair { host: String },
    /// Call one tool on one host and return the result (for the CLI and scripts).
    Call { host: Option<String>, tool: String, args: serde_json::Map<String, Value> },
    /// Upgrade this connection to an aggregated MCP session.
    Mcp,
    Shutdown,
}

/// State shared by every client of the agent.
pub(crate) struct Agent {
    pub mesh: Mesh,
    hosts_path: PathBuf,
    conns: Mutex<HashMap<String, Connection>>,
    active: AtomicUsize,
    last_activity: std::sync::Mutex<Instant>,
}

impl Agent {
    /// A live connection to `host`, dialing (or redialing) when needed. One connection per host is
    /// shared by all sessions: each MCP session is a stream on it.
    pub async fn connection(&self, host: &str) -> Result<Connection> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(host) {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
            conns.remove(host);
        }
        let conn = self
            .mesh
            .dial(host, ALPN_MCP)
            .await
            .with_context(|| format!("cannot reach host '{host}' (is `ai-remote host` running there?)"))?;
        conns.insert(host.to_string(), conn.clone());
        Ok(conn)
    }

    /// Drops a cached connection (after a transport error) so the next use redials.
    pub async fn invalidate(&self, host: &str) {
        self.conns.lock().await.remove(host);
    }

    /// Paired hosts, by local name.
    pub async fn host_names(&self) -> Result<Vec<String>> {
        Ok(self.mesh.peers().await?.into_iter().map(|p| p.name).collect())
    }

    pub fn known_hosts(&self) -> KnownHosts {
        KnownHosts::load(&self.hosts_path).unwrap_or_default()
    }

    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    pub async fn path_of(&self, host: &str) -> Option<PathKind> {
        let conn = self.conns.lock().await.get(host).cloned()?;
        if conn.close_reason().is_some() {
            return None;
        }
        Some(Mesh::path(&conn).await)
    }
}

/// Runs the agent until idle for a long time, Ctrl+C, or a `shutdown` request.
pub async fn run_agent(profile: Profile) -> Result<()> {
    let endpoint = profile.control_endpoint(Role::Controller)?;
    let mut listener = ipc::Listener::bind(&endpoint).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!("a controller agent is already running for this profile")
        } else {
            anyhow!("bind control endpoint {}: {e}", endpoint.display())
        }
    })?;
    let device_key = identity::load_or_create(&profile.identity_path(Role::Controller)?)?;
    let mesh = Mesh::start(MeshOptions {
        root: profile.mesh_dir(Role::Controller)?,
        device_key,
        nickname: machine_name(),
        relays: Vec::new(),
    })
    .await?;
    let agent = Arc::new(Agent {
        mesh,
        hosts_path: profile.role_dir(Role::Controller)?.join("hosts.json"),
        conns: Mutex::new(HashMap::new()),
        active: AtomicUsize::new(0),
        last_activity: std::sync::Mutex::new(Instant::now()),
    });
    tracing::info!(device = %agent.mesh.short_id(), "controller agent running");

    let shutdown = CancellationToken::new();
    let mut idle_check = tokio::time::interval(Duration::from_secs(60));
    loop {
        tokio::select! {
            conn = listener.accept() => match conn {
                Ok(conn) => {
                    let agent = agent.clone();
                    let shutdown = shutdown.clone();
                    agent.active.fetch_add(1, Ordering::SeqCst);
                    agent.touch();
                    tokio::spawn(async move {
                        if let Err(e) = serve_client(&agent, conn, &shutdown).await {
                            tracing::debug!(%e, "agent client ended with an error");
                        }
                        agent.active.fetch_sub(1, Ordering::SeqCst);
                        agent.touch();
                    });
                }
                Err(e) => tracing::warn!(%e, "control connection failed"),
            },
            _ = idle_check.tick() => {
                let idle = agent.last_activity.lock().unwrap().elapsed();
                if agent.active.load(Ordering::SeqCst) == 0 && idle >= IDLE_EXIT {
                    tracing::info!("no clients for {} minutes; exiting", IDLE_EXIT.as_secs() / 60);
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = shutdown.cancelled() => break,
        }
    }
    Ok(())
}

async fn serve_client(agent: &Arc<Agent>, mut conn: ipc::Conn, shutdown: &CancellationToken) -> Result<()> {
    while let Some(line) = conn.read_json().await? {
        agent.touch();
        let req: AgentRequest = match serde_json::from_value(line) {
            Ok(r) => r,
            Err(e) => {
                conn.reply(Err(anyhow!("bad request: {e}"))).await?;
                continue;
            }
        };
        if let AgentRequest::Mcp = req {
            conn.reply(Ok(json!({"mcp": true}))).await?;
            let (stream, buffered) = conn.into_raw();
            return Aggregator::new(agent.clone()).serve(stream, buffered).await;
        }
        let result = handle(agent, req, shutdown).await;
        conn.reply(result).await?;
    }
    Ok(())
}

async fn handle(agent: &Arc<Agent>, req: AgentRequest, shutdown: &CancellationToken) -> Result<Value> {
    Ok(match req {
        AgentRequest::Status => json!({
            "device": agent.mesh.principal(),
            "version": VERSION,
            "hosts": agent.host_names().await?,
        }),
        AgentRequest::Pair { ticket, name } => {
            let paired = agent.mesh.pair(&ticket, name.as_deref()).await?;
            let mut known = agent.known_hosts();
            known.upsert(KnownHost { name: paired.peer.clone(), level: paired.level, paired_at: unix_now() });
            known.save(&agent.hosts_path)?;
            serde_json::to_value(paired)?
        }
        AgentRequest::Hosts => {
            let known = agent.known_hosts();
            let mut out = Vec::new();
            for name in agent.host_names().await? {
                let k = known.get(&name);
                out.push(json!({
                    "name": name,
                    "level_at_pairing": k.and_then(|k| k.level),
                    "paired_at": k.map(|k| k.paired_at),
                    "path": agent.path_of(&name).await,
                }));
            }
            Value::Array(out)
        }
        AgentRequest::Unpair { host } => {
            let peer = agent.mesh.find_peer(&host).await?;
            agent.invalidate(&peer.name).await;
            agent.mesh.forget(&peer).await?;
            let mut known = agent.known_hosts();
            known.remove(&peer.name);
            known.save(&agent.hosts_path)?;
            json!({"host": peer.name, "unpaired": true})
        }
        AgentRequest::Call { host, tool, args } => {
            let aggregator = Aggregator::new(agent.clone());
            let host = match host {
                Some(h) => h,
                None => aggregator.only_host().await?,
            };
            let result = aggregator.call_on_host(&host, &tool, args).await?;
            serde_json::to_value(result)?
        }
        AgentRequest::Mcp => bail!("unreachable"),
        AgentRequest::Shutdown => {
            shutdown.cancel();
            json!({"stopping": true})
        }
    })
}
