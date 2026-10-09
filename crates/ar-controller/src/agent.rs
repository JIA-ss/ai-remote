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
use ar_core::{Profile, Role, VERSION, identity, machine_name, unix_now};
use ar_net::{
    ALPN_EVENTS, ALPN_MCP, ALPN_SCREEN, Mesh, MeshOptions, PathKind, ipc, iroh::endpoint::Connection, refusal::Refusal,
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

/// State shared by every client of the agent.
pub(crate) struct Agent {
    pub mesh: Mesh,
    hosts_path: PathBuf,
    conns: Mutex<HashMap<String, Connection>>,
    refusals: std::sync::Mutex<Refusals>,
    active: AtomicUsize,
    last_activity: std::sync::Mutex<Instant>,
    /// Host sessions for a person's own calls (the app while someone drives), kept between calls so
    /// each click does not start a new session.
    person: std::sync::OnceLock<Aggregator>,
    /// The agents (MCP clients) connected now.
    pub(crate) sessions: crate::sessions::AgentSessions,
}

impl Agent {
    /// A live connection to `host`, dialing (or redialing) when needed. One connection per host is
    /// shared by all sessions: each MCP session is a stream on it.
    pub async fn connection(&self, host: &str) -> Result<Connection> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(host) {
            let Some(reason) = c.close_reason() else {
                return Ok(c.clone());
            };
            conns.remove(host);
            // Report a refusal once instead of redialing straight into it; the next use dials
            // again, so a fix on the host (unrevoke, a granted level) is picked up.
            let refusal = Refusal::from_connection_error(&reason);
            if refusal.is_final() {
                return Err(self.refused(host, refusal));
            }
            self.note_refusal(host, refusal);
        }
        let conn = match self.mesh.dial(host, ALPN_MCP).await {
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
        conns.insert(host.to_string(), conn.clone());
        Ok(conn)
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
        refusals: std::sync::Mutex::new(Refusals::default()),
        active: AtomicUsize::new(0),
        last_activity: std::sync::Mutex::new(Instant::now()),
        person: std::sync::OnceLock::new(),
        sessions: Default::default(),
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
            let (id, stop) = agent.sessions.open();
            let served = tokio::select! {
                served = Aggregator::new(agent.clone()).with_session(id).serve(stream, buffered) => served,
                // Disconnected from the app: dropping the session closes its stream.
                _ = stop.cancelled() => Ok(()),
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
                    feed.relay(stream).await;
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
                    view.relay(stream, buffered).await;
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
        AgentRequest::Status => json!({
            "device": agent.mesh.principal(),
            "name": machine_name(),
            "version": VERSION,
            "hosts": agent.host_names().await?,
            "agents": agent.sessions.list(),
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
                out.push(json!({
                    "name": name,
                    "device": device,
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
            agent.clear_refusal(&peer.name);
            agent.mesh.forget(&peer).await?;
            let mut known = agent.known_hosts();
            known.remove(&peer.name);
            known.save(&agent.hosts_path)?;
            json!({"host": peer.name, "unpaired": true})
        }
        AgentRequest::Call { host, tool, args, person } => {
            let aggregator = if person {
                agent.person.get_or_init(|| Aggregator::for_person(agent.clone())).clone()
            } else {
                Aggregator::new(agent.clone())
            };
            let host = match host {
                Some(h) => h,
                None => aggregator.only_host().await?,
            };
            let result = aggregator
                .call_on_host(&host, &tool, args, &CancellationToken::new())
                .await
                .map_err(|e| anyhow!("host '{host}': {e:#}"))?;
            serde_json::to_value(result)?
        }
        AgentRequest::Channel { host, request } => channel_request(agent, host, &request).await?,
        AgentRequest::Mcp | AgentRequest::Events { .. } | AgentRequest::Screen { .. } => bail!("unreachable"),
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
        None => Aggregator::new(agent.clone()).only_host().await?,
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
        None => Aggregator::new(agent.clone()).only_host().await?,
    };
    let conn = agent
        .mesh
        .dial(&host, ALPN_SCREEN)
        .await
        .map_err(|e| anyhow!("cannot reach host '{host}' (is `ai-remote host` running there?): {e:#}"))?;
    let (mut send, recv) = conn.open_bi().await.map_err(|e| anyhow!("host '{host}': {e}"))?;
    let request = if request.is_null() { json!({}) } else { request.clone() };
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
        bail!("host '{host}': {}", reply["error"].as_str().unwrap_or("refused"));
    }
    reply["host"] = host.into();
    Ok((ScreenView { conn, send, recv }, reply))
}

impl ScreenView {
    /// Copies video to the local client and the client's lines to the host, until either ends.
    async fn relay(self, local: ipc::LocalStream, buffered: Vec<u8>) {
        let ScreenView { conn, mut send, mut recv } = self;
        let (mut local_read, mut local_write) = tokio::io::split(local);
        let up = async {
            if !buffered.is_empty() && send.write_all(&buffered).await.is_err() {
                return;
            }
            let _ = tokio::io::copy(&mut local_read, &mut send).await;
        };
        tokio::select! {
            _ = tokio::io::copy(&mut recv, &mut local_write) => {}
            _ = up => {}
        }
        conn.close(0u32.into(), b"done");
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
        None => Aggregator::new(agent.clone()).only_host().await?,
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
    async fn relay(self, local: ipc::LocalStream) {
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
}
