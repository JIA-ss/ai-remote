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

use anyhow::{Result, anyhow, bail};
use ar_core::{Profile, Role, VERSION, config::ControllerConfig, identity, machine_name, unix_now};
use ar_net::{
    ALPN_EVENTS, ALPN_FILES, ALPN_INPUT, ALPN_MCP, ALPN_SCREEN, Mesh, MeshOptions, NetworkOptions, Route, ipc,
    iroh::endpoint::Connection, refusal::Refusal,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{io::AsyncReadExt, sync::Mutex};
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
    /// This device, the version, the paired hosts and the agents (MCP clients) connected now.
    Status,
    /// Cut off one connected agent (an id from `status`).
    DisconnectAgent {
        id: u64,
    },
    /// Redeem a host's pairing ticket.
    Pair {
        ticket: String,
        name: Option<String>,
    },
    Hosts,
    /// Remove the firewall's inbound block rules for this program and allow inbound UDP (asks for
    /// an administrator's consent on Windows).
    FixFirewall,
    /// Forget a host (its pairing on this controller).
    Unpair {
        host: String,
    },
    /// Call one tool on one host and return the result (for the CLI, scripts and the app).
    Call {
        host: Option<String>,
        tool: String,
        args: serde_json::Map<String, Value>,
        /// The call comes from a person at this device (the app while someone drives), so it runs
        /// while they have taken over.
        #[serde(default)]
        person: bool,
    },
    /// Upgrade this connection to an aggregated MCP session.
    Mcp,
    /// Upgrade this connection to a host's live activity feed (one JSON line per event).
    Events {
        host: Option<String>,
    },
    /// Upgrade this connection to a host's live view: after the host's JSON reply, its binary video
    /// frames (see the host's screen protocol); lines written on this connection reach the host
    /// (`{"keyframe":true}`).
    Screen {
        host: Option<String>,
        /// The host's screen request (`monitor`, `max_fps`, `max_long_edge`, `bitrate_kbps`).
        #[serde(default)]
        request: Value,
    },
    /// One request on a host's events channel, returning the host's reply: approvals (`pending`,
    /// `answer`, `enroll`, `approver_status`), turns (`turn`, `tell`) and this device's access
    /// (`access`, `lower`, `leave`); see the host's events protocol. Allowing needs a signature
    /// the caller made with the person's approver key; the agent never signs, and no MCP tool
    /// reaches this.
    Channel {
        host: Option<String>,
        request: Value,
    },
    /// Upgrade this connection to a file transfer: `request` (`{"ticket":".."}`, from a `files_push`
    /// or `files_pull` call) goes to the host's files channel; after the host's JSON reply, the bytes
    /// pass both ways unchanged (see the host's files protocol).
    Files {
        host: Option<String>,
        request: Value,
    },
    /// Upgrade this connection to a person's live input on a host: `request` (`{"clipboard":true}`)
    /// goes to the host's input channel; after the host's JSON reply, JSON lines pass both ways
    /// unchanged (see the host's input protocol). For the app while someone drives.
    Input {
        host: Option<String>,
        #[serde(default)]
        request: Value,
    },
    /// Set (or with `None`, clear) the host calls go to when they name none.
    SetDefault {
        host: Option<String>,
    },
    /// Replace the addresses a host is dialled at (`ip`, `ip:port`, several separated by commas):
    /// in LAN mode (`[network] mode = "lan"`) the only way to reach a host whose address changed.
    SetHostAddress {
        host: String,
        address: String,
    },
    Shutdown,
}

/// The last definitive refusal of each host, kept until a session succeeds again. A revoked device
/// hears "access revoked" only when its live connection is cut; every later dial meets the gate's
/// generic "unauthorized", and this keeps the specific reason.
#[derive(Default)]
struct Refusals(HashMap<String, Refusal>);

impl Refusals {
    /// Explains `refusal` in the light of earlier ones, and remembers it when it is definitive.
    fn note(&mut self, host: &str, refusal: Refusal) -> Refusal {
        let refusal = match (refusal, self.0.get(host)) {
            (Refusal::NotPaired, Some(Refusal::Revoked)) => Refusal::Revoked,
            (r, _) => r,
        };
        if refusal.is_final() {
            self.0.insert(host.to_string(), refusal.clone());
        }
        refusal
    }
}

/// Pauses before redialing an unreachable host.
const DIAL_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];
/// No retry would start past this much dialing: one dial waits up to 20 s for its paths, and a dial
/// that used all of that gave every path its chance.
const DIAL_RETRY_BUDGET: Duration = Duration::from_secs(20);

/// How long to wait before dial attempt `attempt + 1`, or `None` to give up.
fn retry_after(refusal: &Refusal, attempt: usize, elapsed: Duration) -> Option<Duration> {
    let delay = *DIAL_RETRY_DELAYS.get(attempt)?;
    (matches!(refusal, Refusal::Unreachable) && elapsed + delay < DIAL_RETRY_BUDGET).then_some(delay)
}

/// State shared by every client of the agent.
pub(crate) struct Agent {
    pub mesh: Mesh,
    hosts_path: PathBuf,
    conns: Mutex<HashMap<String, Connection>>,
    /// Whether the firewall blocks this program, read at most once a minute.
    firewall: Arc<crate::firewall::FirewallCache>,
    /// One dial at a time per host, without holding `conns` (and every other host) while it runs.
    dialing: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    refusals: std::sync::Mutex<Refusals>,
    active: AtomicUsize,
    last_activity: std::sync::Mutex<Instant>,
    /// Host sessions for a person's own calls (the app while someone drives), kept between calls so
    /// each click does not start a new session.
    person: std::sync::Mutex<Option<Aggregator>>,
    /// The agents (MCP clients) connected now.
    pub(crate) sessions: crate::sessions::AgentSessions,
}

impl Agent {
    /// A live connection to `host`, dialing (or redialing) when needed. One connection per host is
    /// shared by all sessions: each MCP session is a stream on it.
    pub async fn connection(&self, host: &str) -> Result<Connection> {
        if let Some(c) = self.cached(host).await? {
            return Ok(c);
        }
        let gate = self.dialing.lock().unwrap().entry(host.to_string()).or_default().clone();
        let _dialing = gate.lock().await;
        // Another caller may have connected while this one waited for the gate.
        if let Some(c) = self.cached(host).await? {
            return Ok(c);
        }
        let conn = match self.dial(host).await {
            Ok(conn) => conn,
            Err(e) => {
                let cause = format!("{e:#}");
                tracing::debug!(host, error = %cause, "dial failed");
                let refusal = Refusal::from_dial_error(&e);
                // "Unreachable" covers everything unrecognised: the raw cause tells this machine
                // being offline from a blocked relay or a dial timeout.
                if matches!(refusal, Refusal::Unreachable) {
                    let err = self.refused(host, refusal);
                    return Err(anyhow!("{err} (cause: {cause})"));
                }
                return Err(self.refused(host, refusal));
            }
        };
        self.conns.lock().await.insert(host.to_string(), conn.clone());
        Ok(conn)
    }

    /// The open cached connection to `host`. A closed one is dropped, and a refusal it ended with
    /// is reported once instead of redialing straight into it; the next use dials again, so a fix
    /// on the host (unrevoke, a granted level) is picked up.
    async fn cached(&self, host: &str) -> Result<Option<Connection>> {
        let mut conns = self.conns.lock().await;
        let Some(c) = conns.get(host) else { return Ok(None) };
        let Some(reason) = c.close_reason() else { return Ok(Some(c.clone())) };
        conns.remove(host);
        let refusal = Refusal::from_connection_error(&reason);
        if refusal.is_final() {
            return Err(self.refused(host, refusal));
        }
        self.note_refusal(host, refusal);
        Ok(None)
    }

    /// Dials `host`, trying again after 1 s and 3 s while it is unreachable: a network that is just
    /// switching, a relay reconnecting or a fresh address lookup often gets through a moment later.
    /// Refusals (revoked, not paired) are answers, not failures, and are never retried; nor is a
    /// dial that already ran for [`DIAL_RETRY_BUDGET`], which gave every path its chance.
    async fn dial(&self, host: &str) -> Result<Connection> {
        let started = Instant::now();
        let mut attempt = 0;
        loop {
            let err = match self.mesh.dial(host, ALPN_MCP).await {
                Ok(conn) => return Ok(conn),
                Err(e) => e,
            };
            let Some(delay) = retry_after(&Refusal::from_dial_error(&err), attempt, started.elapsed()) else {
                return Err(err);
            };
            tracing::debug!(host, attempt, error = %format!("{err:#}"), "dial failed; retrying");
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    /// Whether a connection to `host` is kept from earlier and not known to be closed.
    pub async fn has_connection(&self, host: &str) -> bool {
        self.conns.lock().await.get(host).is_some_and(|c| c.close_reason().is_none())
    }

    /// Drops a cached connection (after a transport error) so the next use redials.
    pub async fn invalidate(&self, host: &str) {
        self.conns.lock().await.remove(host);
    }

    /// Why the cached connection to `host` ended, if it did.
    pub async fn closed_because(&self, host: &str) -> Option<Refusal> {
        let conn = self.conns.lock().await.get(host).cloned()?;
        let reason = conn.close_reason()?;
        Some(self.note_refusal(host, Refusal::from_connection_error(&reason)))
    }

    fn note_refusal(&self, host: &str, refusal: Refusal) -> Refusal {
        self.refusals.lock().unwrap().note(host, refusal)
    }

    /// The host served a session again: forget old refusals.
    pub fn clear_refusal(&self, host: &str) {
        self.refusals.lock().unwrap().0.remove(host);
    }

    /// An error carrying the plain-language reason a host refused or could not be reached (callers
    /// name the host).
    pub fn refused(&self, host: &str, refusal: Refusal) -> anyhow::Error {
        let refusal = self.note_refusal(host, refusal);
        anyhow!("{refusal}{}", self.device_hint(&refusal))
    }

    /// Like [`refused`](Self::refused), for a call that was running when the connection ended.
    pub fn refused_during_call(&self, host: &str, refusal: Refusal) -> anyhow::Error {
        let refusal = self.note_refusal(host, refusal);
        anyhow!("{}{}", refusal.during_call(), self.device_hint(&refusal))
    }

    /// The host owner finds this controller by its device id: its name on the host is the host's
    /// choice, and a forgotten device has none left there.
    fn device_hint(&self, refusal: &Refusal) -> String {
        if refusal.needs_host_owner() {
            format!(". This controller's device id: {}", self.mesh.principal())
        } else {
            String::new()
        }
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

    /// While `route` is relayed: what on this side's network likely kept it from going direct
    /// (`"udp_blocked"`, `"symmetric_nat"`, `"captive_portal"`), or `"remote"` when nothing here
    /// explains it, so the other side's network is the likely cause.
    /// `"firewall"` when the system firewall refuses inbound traffic for this program.
    pub async fn p2p_obstacle(&self, route: Option<&Route>) -> Option<&'static str> {
        if route?.path != ar_net::PathKind::Relay {
            return None;
        }
        if let Some(obstacle) = self.mesh.nat().and_then(|n| n.obstacle()) {
            return Some(obstacle);
        }
        let firewall = self.firewall.clone();
        let blocked = tokio::task::spawn_blocking(move || firewall.blocked()).await.unwrap_or(false);
        Some(if blocked { "firewall" } else { "remote" })
    }

    /// The route of the open connection to `host`: direct or relayed, and through which relay.
    pub async fn route_of(&self, host: &str) -> Option<Route> {
        let conn = self.conns.lock().await.get(host).cloned()?;
        if conn.close_reason().is_some() {
            return None;
        }
        Some(Mesh::route(&conn).await)
    }
}

/// Runs the agent until idle for a long time, Ctrl+C, or a `shutdown` request.
async fn start_state(profile: &Profile) -> Result<Arc<Agent>> {
    let device_key = identity::load_or_create(&profile.identity_path(Role::Controller)?)?;
    let mesh = Mesh::start(MeshOptions {
        root: profile.mesh_dir(Role::Controller)?,
        device_key,
        nickname: machine_name(),
        network: NetworkOptions::from_config(
            &ControllerConfig::load(&profile.controller_config_path()?)?.network,
            Role::Controller,
        )?,
    })
    .await?;
    let agent = Arc::new(Agent {
        mesh,
        hosts_path: profile.role_dir(Role::Controller)?.join("hosts.json"),
        conns: Mutex::new(HashMap::new()),
        dialing: std::sync::Mutex::new(HashMap::new()),
        firewall: Default::default(),
        refusals: std::sync::Mutex::new(Refusals::default()),
        active: AtomicUsize::new(0),
        last_activity: std::sync::Mutex::new(Instant::now()),
        person: std::sync::Mutex::new(None),
        sessions: Default::default(),
    });
    tracing::info!(device = %agent.mesh.short_id(), "controller agent running");

    Ok(agent)
}

/// The native mobile controller uses the same handlers, without subprocesses or filesystem
/// sockets (iOS application-container paths exceed sockaddr_un's 103-byte limit).
pub struct EmbeddedController {
    agent: std::sync::Mutex<Option<Arc<Agent>>>,
    shutdown: CancellationToken,
    streams: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl EmbeddedController {
    pub async fn start(profile: &Profile) -> Result<Self> {
        Ok(Self {
            agent: std::sync::Mutex::new(Some(start_state(profile).await?)),
            shutdown: CancellationToken::new(),
            streams: Default::default(),
        })
    }

    fn state(&self) -> Result<Arc<Agent>> {
        if self.shutdown.is_cancelled() {
            bail!("controller is closed");
        }
        self.agent.lock().unwrap().clone().ok_or_else(|| anyhow!("controller is closed"))
    }

    pub async fn request(&self, request: Value) -> Result<Value> {
        let agent = self.state()?;
        let request = serde_json::from_value(request)?;
        tokio::select! {
            _ = self.shutdown.cancelled() => Err(anyhow!("controller is closed")),
            result = handle(&agent, request, &self.shutdown) => result,
        }
    }

    pub async fn open_stream(&self, request: Value) -> Result<(tokio::io::DuplexStream, Value)> {
        let agent = self.state()?;
        let req: AgentRequest = serde_json::from_value(request)?;
        let (client, server) = tokio::io::duplex(64 * 1024);
        let stop = self.shutdown.clone();
        let (reply, task) = tokio::select! {
            _ = stop.cancelled() => return Err(anyhow!("controller is closed")),
            result = async {
                Ok::<_, anyhow::Error>(match req {
                    AgentRequest::Events { host } => {
                        let feed = open_events(&agent, host).await?;
                        let reply = json!({"host":feed.host, "events":true});
                        let cancelled = stop.clone();
                        (reply, tokio::spawn(async move { tokio::select! { _ = feed.relay(server) => {}, _ = cancelled.cancelled() => {} } }))
                    }
                    AgentRequest::Files { host, request } => {
                        let (transfer, reply) = open_files(&agent, host, &request).await?;
                        let cancelled = stop.clone();
                        (reply, tokio::spawn(async move { tokio::select! { _ = transfer.relay(server, vec![]) => {}, _ = cancelled.cancelled() => {} } }))
                    }
                    AgentRequest::Screen { host, request } => {
                        let (view, reply) = open_screen(&agent, host, &request).await?;
                        let cancelled = stop.clone();
                        (reply, tokio::spawn(async move { tokio::select! { _ = view.relay(server, vec![]) => {}, _ = cancelled.cancelled() => {} } }))
                    }
                    _ => bail!("not a streaming request"),
                })
            } => result?,
        };
        let mut tasks = self.streams.lock().unwrap();
        tasks.retain(|t| !t.is_finished());
        if stop.is_cancelled() {
            task.abort();
            bail!("controller is closed");
        }
        tasks.push(task);
        Ok((client, reply))
    }

    pub async fn close(&self) -> Result<()> {
        self.shutdown.cancel();
        let tasks = std::mem::take(&mut *self.streams.lock().unwrap());
        for task in tasks {
            let _ = task.await;
        }
        let agent = self.agent.lock().unwrap().take();
        if let Some(mut agent) = agent {
            agent.person.lock().unwrap().take();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match Arc::try_unwrap(agent) {
                    Ok(agent) => {
                        agent.mesh.shutdown().await;
                        break;
                    }
                    Err(shared) => {
                        // Cancelled requests must first get a chance to drop their state borrow.
                        // Retain ownership on timeout so a later close can actually retry shutdown.
                        if tokio::time::Instant::now() >= deadline {
                            *self.agent.lock().unwrap() = Some(shared);
                            bail!("controller has an in-flight operation; retry close after it ends");
                        }
                        agent = shared;
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
        Ok(())
    }
}

impl Drop for EmbeddedController {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for task in self.streams.get_mut().unwrap().drain(..) {
            task.abort();
        }
        if let Some(agent) = self.agent.get_mut().unwrap().take() {
            agent.person.lock().unwrap().take();
        }
    }
}

pub async fn run_agent(profile: Profile) -> Result<()> {
    let endpoint = profile.control_endpoint(Role::Controller)?;
    let mut listener = ipc::Listener::bind(&endpoint).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!("a controller agent is already running for this profile")
        } else {
            anyhow!("bind control endpoint {}: {e}", endpoint.display())
        }
    })?;
    let agent = start_state(&profile).await?;

    let shutdown = CancellationToken::new();
    let mut idle_check = tokio::time::interval(Duration::from_secs(60));
    let mut clients = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            conn = listener.accept() => match conn {
                Ok(conn) => {
                    let agent = agent.clone();
                    let shutdown = shutdown.clone();
                    agent.active.fetch_add(1, Ordering::SeqCst);
                    agent.touch();
                    clients.spawn(async move {
                        if let Err(e) = serve_client(&agent, conn, &shutdown).await {
                            tracing::debug!(%e, "agent client ended with an error");
                        }
                        agent.active.fetch_sub(1, Ordering::SeqCst);
                        agent.touch();
                    });
                }
                Err(e) => tracing::warn!(%e, "control connection failed"),
            },
            _ = clients.join_next(), if !clients.is_empty() => {},
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
    // A mobile app embeds this agent rather than exiting a process. Stop all local clients,
    // break the cached person's Arc cycle, then release the node's database and UDP endpoint.
    shutdown.cancel();
    if tokio::time::timeout(Duration::from_secs(5), async { while clients.join_next().await.is_some() {} })
        .await
        .is_err()
    {
        clients.abort_all();
        while clients.join_next().await.is_some() {}
    }
    agent.person.lock().unwrap().take();
    let agent = Arc::try_unwrap(agent).map_err(|_| anyhow!("controller still shared after shutdown"))?;
    agent.mesh.shutdown().await;
    Ok(())
}

async fn serve_client(agent: &Arc<Agent>, mut conn: ipc::Conn, shutdown: &CancellationToken) -> Result<()> {
    loop {
        let line = tokio::select! {
            _ = shutdown.cancelled() => break,
            line = conn.read_json() => line?,
        };
        let Some(line) = line else { break };
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
            let (id, stop) = agent.sessions.open();
            let served = tokio::select! {
                served = Aggregator::new(agent.clone()).with_session(id).serve(stream, buffered) => served,
                // Disconnected from the app: dropping the session closes its stream.
                _ = stop.cancelled() => Ok(()),
                _ = shutdown.cancelled() => Ok(()),
            };
            agent.sessions.close(id);
            return served;
        }
        if let AgentRequest::Events { host } = req {
            // Connect first, so a failure is still an ordinary error reply.
            match open_events(agent, host).await {
                Ok(feed) => {
                    conn.reply(Ok(json!({"events": true, "host": feed.host}))).await?;
                    let (stream, _) = conn.into_raw();
                    tokio::select! {
                        _ = feed.relay(stream) => {},
                        _ = shutdown.cancelled() => {},
                    }
                    return Ok(());
                }
                Err(e) => {
                    conn.reply(Err(e)).await?;
                    continue;
                }
            }
        }
        if let AgentRequest::Screen { host, request } = req {
            match open_screen(agent, host, &request).await {
                Ok((view, reply)) => {
                    conn.reply(Ok(reply)).await?;
                    let (stream, buffered) = conn.into_raw();
                    tokio::select! {
                        _ = view.relay(stream, buffered) => {},
                        _ = shutdown.cancelled() => {},
                    }
                    return Ok(());
                }
                Err(e) => {
                    conn.reply(Err(e)).await?;
                    continue;
                }
            }
        }
        if let AgentRequest::Files { host, request } = req {
            match open_files(agent, host, &request).await {
                Ok((transfer, reply)) => {
                    conn.reply(Ok(reply)).await?;
                    let (stream, buffered) = conn.into_raw();
                    tokio::select! {
                        _ = transfer.relay(stream, buffered) => {},
                        _ = shutdown.cancelled() => {},
                    }
                    return Ok(());
                }
                Err(e) => {
                    conn.reply(Err(e)).await?;
                    continue;
                }
            }
        }
        if let AgentRequest::Input { host, request } = req {
            match open_input(agent, host, &request).await {
                Ok((input, reply)) => {
                    conn.reply(Ok(reply)).await?;
                    let (stream, buffered) = conn.into_raw();
                    tokio::select! {
                        _ = input.relay(stream, buffered) => {},
                        _ = shutdown.cancelled() => {},
                    }
                    return Ok(());
                }
                Err(e) => {
                    conn.reply(Err(e)).await?;
                    continue;
                }
            }
        }
        let result = handle(agent, req, shutdown).await;
        conn.reply(result).await?;
    }
    Ok(())
}

async fn handle(agent: &Arc<Agent>, req: AgentRequest, shutdown: &CancellationToken) -> Result<Value> {
    Ok(match req {
        AgentRequest::FixFirewall => {
            tokio::task::spawn_blocking(crate::firewall::fix).await??;
            agent.firewall.clear();
            json!({"fixed": true})
        }
        AgentRequest::Status => json!({
            "device": agent.mesh.principal(),
            "name": machine_name(),
            "version": VERSION,
            "hosts": agent.host_names().await?,
            "agents": agent.sessions.list(),
            "lan_direct": agent.mesh.lan_direct(),
        }),
        AgentRequest::DisconnectAgent { id } => json!({"disconnected": agent.sessions.stop(id)}),
        AgentRequest::Pair { ticket, name } => {
            let paired = agent.mesh.pair(&ticket, name.as_deref()).await?;
            // A fresh pairing supersedes whatever this host refused before.
            agent.clear_refusal(&paired.peer);
            agent.invalidate(&paired.peer).await;
            let mut known = agent.known_hosts();
            known.upsert(KnownHost { name: paired.peer.clone(), level: paired.level, paired_at: unix_now() });
            known.save(&agent.hosts_path)?;
            serde_json::to_value(paired)?
        }
        AgentRequest::Hosts => {
            let known = agent.known_hosts();
            let mut out = Vec::new();
            let peers = agent.mesh.peers().await?;
            for name in agent.host_names().await? {
                let k = known.get(&name);
                let device = peers.iter().find(|p| p.name == name).and_then(|p| p.principal.clone());
                let route = agent.route_of(&name).await;
                out.push(json!({
                    "name": name,
                    "device": device,
                    "level_at_pairing": k.and_then(|k| k.level),
                    "paired_at": k.map(|k| k.paired_at),
                    "path": route.as_ref().map(|r| r.path),
                    "relay": route.as_ref().and_then(|r| r.relay.clone()),
                    "relay_n0": route.as_ref().is_some_and(Route::via_n0),
                    "p2p_obstacle": agent.p2p_obstacle(route.as_ref()).await,
                    "addresses": agent.mesh.peer_addrs(&name).await.unwrap_or_default().iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "default": known.default.as_deref() == Some(name.as_str()),
                }));
            }
            Value::Array(out)
        }
        AgentRequest::SetDefault { host } => {
            let name = match host {
                Some(h) => Some(agent.mesh.find_peer(&h).await?.name),
                None => None,
            };
            let mut known = agent.known_hosts();
            known.default = name.clone();
            known.save(&agent.hosts_path)?;
            json!({"default": name})
        }
        AgentRequest::SetHostAddress { host, address } => {
            let peer = agent.mesh.find_peer(&host).await?;
            let addrs = ar_core::config::parse_host_addresses(&address)?;
            let kept = agent.mesh.set_peer_addrs(&peer.name, &addrs).await?;
            // A kept connection still goes to the old address; the next call dials the new one.
            agent.invalidate(&peer.name).await;
            json!({"host": peer.name, "addresses": kept.iter().map(ToString::to_string).collect::<Vec<_>>()})
        }
        AgentRequest::Unpair { host } => {
            let peer = agent.mesh.find_peer(&host).await?;
            agent.invalidate(&peer.name).await;
            agent.clear_refusal(&peer.name);
            agent.mesh.forget(&peer).await?;
            let mut known = agent.known_hosts();
            known.remove(&peer.name);
            known.save(&agent.hosts_path)?;
            json!({"host": peer.name, "unpaired": true})
        }
        AgentRequest::Call { host, tool, args, person } => {
            let aggregator = if person {
                agent.person.lock().unwrap().get_or_insert_with(|| Aggregator::for_person(agent.clone())).clone()
            } else {
                Aggregator::new(agent.clone())
            };
            let host = match host {
                Some(h) => h,
                None => aggregator.implicit_host().await?,
            };
            let result = aggregator
                .call_on_host(&host, &tool, args, &CancellationToken::new())
                .await
                .map_err(|e| anyhow!("host '{host}': {e:#}"))?;
            serde_json::to_value(result)?
        }
        AgentRequest::Channel { host, request } => channel_request(agent, host, &request).await?,
        AgentRequest::Mcp
        | AgentRequest::Events { .. }
        | AgentRequest::Screen { .. }
        | AgentRequest::Files { .. }
        | AgentRequest::Input { .. } => {
            bail!("unreachable")
        }
        AgentRequest::Shutdown => {
            shutdown.cancel();
            json!({"stopping": true})
        }
    })
}

/// Longest reply accepted on the events channel.
const MAX_CHANNEL_REPLY: u64 = 1 << 20;

/// Sends one request on `host`'s events channel and returns its reply (with the host's name added).
/// An `enroll` waits for someone at the host to confirm, so the wait is long.
async fn channel_request(agent: &Arc<Agent>, host: Option<String>, request: &Value) -> Result<Value> {
    const ALLOWED: [&str; 9] =
        ["pending", "answer", "enroll", "approver_status", "turn", "tell", "access", "lower", "leave"];
    let kind = request.as_object().filter(|o| o.len() == 1).and_then(|o| o.keys().next().cloned());
    if !kind.as_deref().is_some_and(|k| ALLOWED.contains(&k)) {
        bail!("host channel requests are one of: {}", ALLOWED.join(", "));
    }
    let host = match host {
        Some(h) => agent.mesh.find_peer(&h).await?.name,
        None => Aggregator::new(agent.clone()).implicit_host().await?,
    };
    let conn = agent
        .mesh
        .dial(&host, ALPN_EVENTS)
        .await
        .map_err(|e| anyhow!("cannot reach host '{host}' (is `ai-remote host` running there?): {e:#}"))?;
    let (mut send, recv) = conn.open_bi().await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    let mut line = request.to_string();
    line.push('\n');
    send.write_all(line.as_bytes()).await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    let _ = send.finish();
    let mut reply = String::new();
    let read = tokio::time::timeout(
        Duration::from_secs(30 * 60),
        tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(recv.take(MAX_CHANNEL_REPLY)), &mut reply),
    )
    .await;
    conn.close(0u32.into(), b"done");
    match read {
        Ok(Ok(n)) if n > 0 => {}
        Ok(Ok(_)) => bail!("host '{host}' closed the request without an answer (an older host?)"),
        Ok(Err(e)) => bail!("host '{host}': {e}"),
        Err(_) => bail!("host '{host}' did not answer in time"),
    }
    let mut reply: Value = serde_json::from_str(&reply).map_err(|e| anyhow!("host '{host}' sent a bad reply: {e}"))?;
    if reply["ok"] != true {
        let error = reply["error"].as_str().unwrap_or("refused");
        bail!("host '{host}': {error}");
    }
    reply["host"] = host.into();
    Ok(reply)
}

/// A live view of one host.
struct ScreenView {
    conn: Connection,
    send: ar_net::iroh::endpoint::SendStream,
    recv: ar_net::iroh::endpoint::RecvStream,
}

/// Opens `host`'s live view and returns it with the host's first reply (its monitors).
async fn open_screen(agent: &Arc<Agent>, host: Option<String>, request: &Value) -> Result<(ScreenView, Value)> {
    let host = match host {
        Some(h) => agent.mesh.find_peer(&h).await?.name,
        None => Aggregator::new(agent.clone()).implicit_host().await?,
    };
    let conn = agent
        .mesh
        .dial(&host, ALPN_SCREEN)
        .await
        .map_err(|e| anyhow!("cannot reach host '{host}' (is `ai-remote host` running there?): {e:#}"))?;
    let mut request = if request.is_null() { json!({}) } else { request.clone() };
    loop {
        let (mut send, recv) = conn.open_bi().await.map_err(|e| anyhow!("host '{host}': {e}"))?;
        let mut line = request.to_string();
        line.push('\n');
        send.write_all(line.as_bytes()).await.map_err(|e| anyhow!("host '{host}': {e}"))?;
        // The reply line is read byte by byte so no video after it is consumed here.
        let mut recv = recv;
        let mut reply = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            match tokio::time::timeout(Duration::from_secs(15), recv.read_exact(&mut byte)).await {
                Ok(Ok(_)) if byte[0] == b'\n' => break,
                Ok(Ok(_)) if reply.len() < 64 * 1024 => reply.push(byte[0]),
                Ok(Ok(_)) => bail!("host '{host}' sent an oversized reply"),
                Ok(Err(e)) => bail!("host '{host}' closed the live view: {e} (an older host?)"),
                Err(_) => bail!("host '{host}' did not answer"),
            }
        }
        let mut reply: Value = serde_json::from_slice(&reply).map_err(|e| anyhow!("host '{host}': bad reply: {e}"))?;
        if reply["ok"] != true {
            let error = reply["error"].as_str().unwrap_or("refused");
            // A host from before codec negotiation refuses the field; it streams H.264 without it.
            if let Some(older) = without_unknown_field(&request, error) {
                request = older;
                continue;
            }
            bail!("host '{host}': {error}");
        }
        reply["host"] = host.into();
        return Ok((ScreenView { conn, send, recv }, reply));
    }
}

/// The request without the optional field an older host refused as unknown (serde's
/// "unknown field `name`"), when there is one to drop; `None` otherwise.
fn without_unknown_field(request: &Value, error: &str) -> Option<Value> {
    const OPTIONAL: [&str; 2] = ["codecs", "cursor"];
    let field = OPTIONAL.into_iter().find(|f| error.contains(&format!("unknown field `{f}`")))?;
    let mut older = request.clone();
    older.as_object_mut()?.remove(field)?;
    Some(older)
}

impl ScreenView {
    /// Copies video to the local client and the client's lines to the host, until either ends.
    async fn relay<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(self, local: T, buffered: Vec<u8>) {
        let ScreenView { conn, mut send, mut recv } = self;
        let (mut local_read, mut local_write) = tokio::io::split(local);
        let up = async {
            if !buffered.is_empty() && send.write_all(&buffered).await.is_err() {
                return;
            }
            let _ = tokio::io::copy(&mut local_read, &mut send).await;
        };
        let down = async {
            match (test_link_bps(), trace_hops()) {
                (Some(bps), _) => copy_paced(&mut recv, &mut local_write, bps).await,
                (None, Some(trace)) => copy_traced(&mut recv, &mut local_write, trace).await,
                (None, None) => tokio::io::copy(&mut recv, &mut local_write).await.map(|_| ()),
            }
        };
        tokio::select! {
            _ = down => {}
            _ = up => {}
        }
        conn.close(0u32.into(), b"done");
    }
}

/// Measurement only: `AI_REMOTE_TRACE_HOPS=<dir>` makes the agent log, for each live-view frame,
/// when its last byte arrived from the host and when it was written to the local client
/// (`<dir>/agent.tsv`: capture timestamp, then both wall-clock times in µs).
fn trace_hops() -> Option<std::io::BufWriter<std::fs::File>> {
    let dir = std::env::var_os("AI_REMOTE_TRACE_HOPS")?;
    std::fs::File::create(std::path::Path::new(&dir).join("agent.tsv")).ok().map(std::io::BufWriter::new)
}

/// Copies whole live-view messages (18-byte header + body), logging video frames' times.
async fn copy_traced<R, W>(
    read: &mut R,
    write: &mut W,
    mut trace: std::io::BufWriter<std::fs::File>,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::io::Write;
    use tokio::io::AsyncWriteExt;
    let wall = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros());
    let mut message = Vec::new();
    loop {
        let mut header = [0u8; 18];
        read.read_exact(&mut header).await?;
        let len = u32::from_be_bytes([header[14], header[15], header[16], header[17]]) as usize;
        message.clear();
        message.extend_from_slice(&header);
        message.resize(18 + len, 0);
        read.read_exact(&mut message[18..]).await?;
        let arrived = wall();
        write.write_all(&message).await?;
        if header[0] == 1 {
            let ts = u64::from_be_bytes(header[6..14].try_into().unwrap());
            let _ = writeln!(trace, "{ts}\t{arrived}\t{}", wall());
        }
    }
}

/// One file transfer with a host, after its first reply.
struct FileTransfer {
    conn: Connection,
    send: ar_net::iroh::endpoint::SendStream,
    recv: ar_net::iroh::endpoint::RecvStream,
}

/// Redeems a transfer ticket on `host`'s files channel and returns the transfer with the host's
/// first reply (`{"ok":true,"op":"push"|"pull","size":N}`).
async fn open_files(agent: &Arc<Agent>, host: Option<String>, request: &Value) -> Result<(FileTransfer, Value)> {
    let ticket = request["ticket"].as_str().filter(|t| !t.is_empty());
    let Some(ticket) = ticket else { bail!("a file transfer needs the ticket from files_push or files_pull") };
    let host = match host {
        Some(h) => agent.mesh.find_peer(&h).await?.name,
        None => Aggregator::new(agent.clone()).implicit_host().await?,
    };
    let conn = agent
        .mesh
        .dial(&host, ALPN_FILES)
        .await
        .map_err(|e| anyhow!("cannot reach host '{host}' (is `ai-remote host` running there?): {e:#}"))?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    let line = format!("{}\n", json!({ "ticket": ticket }));
    send.write_all(line.as_bytes()).await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    // Byte by byte, so none of the file that follows a pull's reply is consumed here.
    let mut reply = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match tokio::time::timeout(Duration::from_secs(30), recv.read_exact(&mut byte)).await {
            Ok(Ok(_)) if byte[0] == b'\n' => break,
            Ok(Ok(_)) if reply.len() < 64 * 1024 => reply.push(byte[0]),
            Ok(Ok(_)) => bail!("host '{host}' sent an oversized reply"),
            Ok(Err(e)) => bail!("host '{host}' closed the transfer: {e} (a host without file transfers?)"),
            Err(_) => bail!("host '{host}' did not answer"),
        }
    }
    let mut reply: Value = serde_json::from_slice(&reply).map_err(|e| anyhow!("host '{host}': bad reply: {e}"))?;
    if reply["ok"] != true {
        bail!("host '{host}': {}", reply["error"].as_str().unwrap_or("refused"));
    }
    reply["host"] = host.into();
    Ok((FileTransfer { conn, send, recv }, reply))
}

/// A person's live input stream to one host, after the host's first reply.
struct LiveInput {
    conn: Connection,
    send: ar_net::iroh::endpoint::SendStream,
    recv: ar_net::iroh::endpoint::RecvStream,
}

/// Opens `host`'s input channel and returns it with the host's first reply.
async fn open_input(agent: &Arc<Agent>, host: Option<String>, request: &Value) -> Result<(LiveInput, Value)> {
    let host = match host {
        Some(h) => agent.mesh.find_peer(&h).await?.name,
        None => Aggregator::new(agent.clone()).implicit_host().await?,
    };
    let conn = agent.mesh.dial(&host, ALPN_INPUT).await.map_err(|e| {
        anyhow!("cannot open live input on host '{host}' (an older host, or `ai-remote host` not running): {e:#}")
    })?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    let request = if request.is_null() { json!({}) } else { request.clone() };
    let mut line = request.to_string();
    line.push('\n');
    send.write_all(line.as_bytes()).await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    // Byte by byte, so no line after the reply is consumed here.
    let mut reply = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match tokio::time::timeout(Duration::from_secs(15), recv.read_exact(&mut byte)).await {
            Ok(Ok(_)) if byte[0] == b'\n' => break,
            Ok(Ok(_)) if reply.len() < 64 * 1024 => reply.push(byte[0]),
            Ok(Ok(_)) => bail!("host '{host}' sent an oversized reply"),
            Ok(Err(e)) => bail!("host '{host}' closed live input: {e} (an older host?)"),
            Err(_) => bail!("host '{host}' did not answer"),
        }
    }
    let mut reply: Value = serde_json::from_slice(&reply).map_err(|e| anyhow!("host '{host}': bad reply: {e}"))?;
    if reply["ok"] != true {
        bail!("host '{host}': {}", reply["error"].as_str().unwrap_or("refused"));
    }
    reply["host"] = host.into();
    Ok((LiveInput { conn, send, recv }, reply))
}

impl LiveInput {
    /// Passes lines both ways until either side ends. Every write is flushed at once: a pointer
    /// move must not wait in a buffer.
    async fn relay<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(self, local: T, buffered: Vec<u8>) {
        use tokio::io::AsyncWriteExt;
        let LiveInput { conn, mut send, mut recv } = self;
        let (mut local_read, mut local_write) = tokio::io::split(local);
        let up = async {
            if !buffered.is_empty() && send.write_all(&buffered).await.is_err() {
                return;
            }
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match local_read.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if send.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        };
        let down = async {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match recv.read(&mut buf).await {
                    Ok(Some(n)) if n > 0 => {
                        if local_write.write_all(&buf[..n]).await.is_err() || local_write.flush().await.is_err() {
                            return;
                        }
                    }
                    _ => return,
                }
            }
        };
        tokio::select! {
            _ = down => {}
            _ = up => {}
        }
        conn.close(0u32.into(), b"done");
    }
}

impl FileTransfer {
    /// Passes bytes both ways until the host has said everything (or the local client goes away).
    async fn relay<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(self, local: T, buffered: Vec<u8>) {
        let FileTransfer { conn, mut send, recv } = self;
        let (local_read, mut local_write) = tokio::io::split(local);
        // Large buffers: `copy`'s own is a few KiB, which costs a system call per few KiB of a file.
        let (mut local_read, mut recv) = (
            tokio::io::BufReader::with_capacity(1 << 20, local_read),
            tokio::io::BufReader::with_capacity(1 << 20, recv),
        );
        let up = async {
            if !buffered.is_empty() && send.write_all(&buffered).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_buf(&mut local_read, &mut send).await;
        };
        let down = async {
            let _ = tokio::io::copy_buf(&mut recv, &mut local_write).await;
            let _ = tokio::io::AsyncWriteExt::flush(&mut local_write).await;
        };
        tokio::pin!(down);
        tokio::select! {
            _ = &mut down => {}
            // The host stopped reading an upload it refused; its answer saying why is still on its way.
            _ = up => {
                let _ = tokio::time::timeout(Duration::from_secs(30), &mut down).await;
            }
        }
        conn.close(0u32.into(), b"done");
    }
}

/// `AI_REMOTE_TEST_LINK_KBPS`, for tests only: live video is read no faster than this, as a slow
/// network would deliver it. QUIC's own windows then fill as on such a link, so latency and
/// adaptive bitrate can be measured on one machine (`tools/stream-e2e.py --link-mbps`).
fn test_link_bps() -> Option<u64> {
    let kbps: u64 = std::env::var("AI_REMOTE_TEST_LINK_KBPS").ok()?.parse().ok()?;
    (kbps > 0).then_some(kbps * 1000)
}

async fn copy_paced<R, W>(read: &mut R, write: &mut W, bps: u64) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 16 * 1024];
    // When the link is free again; an idle link does not bank time for a later burst.
    let mut free = tokio::time::Instant::now();
    loop {
        let n = read.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        write.write_all(&buf[..n]).await?;
        let now = tokio::time::Instant::now();
        free = free.max(now) + Duration::from_secs_f64(n as f64 * 8.0 / bps as f64);
        // Sleep in steps of at least 20 ms: timers round short sleeps up (to a millisecond or
        // more), which would make the link far slower than `bps` for small reads.
        if free > now + Duration::from_millis(20) {
            tokio::time::sleep_until(free).await;
        }
    }
}

/// A subscribed activity feed of one host.
struct EventFeed {
    host: String,
    /// Kept so the connection lives as long as the feed.
    conn: Connection,
    send: ar_net::iroh::endpoint::SendStream,
    recv: ar_net::iroh::endpoint::RecvStream,
}

async fn open_events(agent: &Arc<Agent>, host: Option<String>) -> Result<EventFeed> {
    let host = match host {
        Some(h) => agent.mesh.find_peer(&h).await?.name,
        None => Aggregator::new(agent.clone()).implicit_host().await?,
    };
    let conn = agent
        .mesh
        .dial(&host, ALPN_EVENTS)
        .await
        .map_err(|e| anyhow!("cannot reach host '{host}' (is `ai-remote host` running there?): {e:#}"))?;
    let (mut send, recv) = conn.open_bi().await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    // A QUIC stream reaches the host only once something is written on it.
    send.write_all(b"{\"subscribe\":true}\n").await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    Ok(EventFeed { host, conn, send, recv })
}

impl EventFeed {
    /// Copies the feed to the local client until the host ends it or the client goes away.
    async fn relay<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(self, local: T) {
        let EventFeed { conn, send, mut recv, .. } = self;
        let (mut local_read, mut local_write) = tokio::io::split(local);
        let client_gone = async {
            let mut buf = [0u8; 256];
            while matches!(local_read.read(&mut buf).await, Ok(n) if n > 0) {}
        };
        tokio::select! {
            _ = tokio::io::copy(&mut recv, &mut local_write) => {}
            _ = client_gone => {}
        }
        drop(send);
        conn.close(0u32.into(), b"done");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_only_an_unreachable_host_and_only_while_there_is_time() {
        let s = Duration::from_secs;
        assert_eq!(retry_after(&Refusal::Unreachable, 0, s(0)), Some(s(1)));
        assert_eq!(retry_after(&Refusal::Unreachable, 1, s(2)), Some(s(3)));
        assert_eq!(retry_after(&Refusal::Unreachable, 2, s(5)), None, "two retries at most");
        assert_eq!(retry_after(&Refusal::Unreachable, 0, s(20)), None, "a timed-out dial is not repeated");
        assert_eq!(retry_after(&Refusal::NotPaired, 0, s(0)), None);
        assert_eq!(retry_after(&Refusal::Revoked, 0, s(0)), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn embedded_close_waits_for_an_in_flight_state_borrow() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(dir.path()).unwrap();
        let controller = EmbeddedController::start(&profile).await.unwrap();
        let borrowed = controller.state().unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            drop(borrowed);
        });
        controller.close().await.unwrap();
        release.await.unwrap();
        assert!(controller.request(json!({"op":"status"})).await.is_err());
        let reopened = EmbeddedController::start(&profile).await.unwrap();
        reopened.close().await.unwrap();
    }

    #[test]
    fn a_revocation_outlives_later_generic_refusals() {
        let mut r = Refusals::default();
        assert_eq!(r.note("office", Refusal::Revoked), Refusal::Revoked);
        // Every later dial meets the gate's "unauthorized".
        assert_eq!(r.note("office", Refusal::NotPaired), Refusal::Revoked);
        assert_eq!(r.note("office", Refusal::NotPaired), Refusal::Revoked);
        // Other hosts are unaffected.
        assert_eq!(r.note("lab", Refusal::NotPaired), Refusal::NotPaired);
    }

    #[test]
    fn transient_failures_are_not_remembered() {
        let mut r = Refusals::default();
        assert_eq!(r.note("office", Refusal::Unreachable), Refusal::Unreachable);
        assert_eq!(r.note("office", Refusal::NotPaired), Refusal::NotPaired);
        assert_eq!(r.note("office", Refusal::NoLevel), Refusal::NoLevel);
        assert!(!r.0.contains_key("lab"));
    }

    #[test]
    fn an_older_host_gets_the_request_without_the_field_it_refused() {
        let request = serde_json::json!({"max_fps": 60, "codecs": ["hevc", "h264"]});
        let older = without_unknown_field(
            &request,
            "bad request: unknown field `codecs`, expected one of `monitor`, `max_fps`, `max_long_edge`, `bitrate_kbps` at line 1 column 30",
        );
        assert_eq!(older, Some(serde_json::json!({"max_fps": 60})));
        // Other refusals are reported, not retried; nor is a field that was not sent.
        assert_eq!(without_unknown_field(&request, "too many people are watching this computer right now"), None);
        assert_eq!(without_unknown_field(&serde_json::json!({"max_fps": 60}), "unknown field `codecs`"), None);
        assert_eq!(without_unknown_field(&request, "unknown field `monitor`"), None);
        // A host from before the separate pointer refuses `cursor` (then perhaps `codecs`): one at a time.
        let both = serde_json::json!({"max_fps": 60, "codecs": ["hevc"], "cursor": "client"});
        let first = without_unknown_field(&both, "bad request: unknown field `cursor`, expected one of").unwrap();
        assert_eq!(first, serde_json::json!({"max_fps": 60, "codecs": ["hevc"]}));
    }
}
