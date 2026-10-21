//! The transport seam of ai-remote.
//!
//! Identity, pairing (one-time invite + a 3-word safety code), the default-deny gate on every
//! connection, revocation that cuts live connections, and dialing by name are provided by
//! [mcpmesh](https://github.com/counterpunchtech/mcpmesh)'s embeddable node. ai-remote carries its
//! own protocols on its own ALPNs over that node, so nothing outside this crate depends on
//! mcpmesh: if it ever has to be replaced, only this crate changes.
//!
//! Permission levels are stored as grants on three marker services (`ai-remote.observe`,
//! `ai-remote.control`, `ai-remote.admin`): an invite names the service of the level it grants, and
//! pairing records the redeemer's principal in that service's allow list.

pub mod ipc;
pub mod proxy;
pub mod refusal;

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Level;
use mcpmesh_local_api::{
    AuditKind, BackendSpec, ControlClient, RecentPairing, StatusResult, StreamFrame, StreamSubscription,
    client::ClientError,
};
use mcpmesh_node::{Node, NodeBuilder, mcpmesh_trust::ed25519_dalek::SigningKey};
use serde::{Deserialize, Serialize};

pub use mcpmesh_local_api::PeerPath;
pub use mcpmesh_node::iroh;

/// ALPN of an MCP session: the controller is the MCP client, the host the MCP server.
pub const ALPN_MCP: &[u8] = b"app/ai-remote/mcp/1";

/// ALPN of the live activity feed (audit lines) a controller subscribes to.
pub const ALPN_EVENTS: &[u8] = b"app/ai-remote/events/1";

/// ALPN of the live view: video of the host's screen for a person at a controller.
pub const ALPN_SCREEN: &[u8] = b"app/ai-remote/screen/1";

/// ALPN of file transfers: the bytes of a file move approved with a `files_push` / `files_pull` call.
pub const ALPN_FILES: &[u8] = b"app/ai-remote/file/1";

/// ALPN of live input: a person's live mouse, keyboard and clipboard while they drive (see `ar-host`'s input channel).
pub const ALPN_INPUT: &[u8] = b"app/ai-remote/input/1";

const SERVICE_PREFIX: &str = "ai-remote.";

fn service_name(level: Level) -> String {
    format!("{SERVICE_PREFIX}{level}")
}

/// How to start a node.
#[derive(Debug, Clone)]
pub struct MeshOptions {
    /// Directory owned by the node (pairings, revocations, outstanding invites).
    pub root: PathBuf,
    /// 32 secret bytes of the device key; the device id derives from it.
    pub device_key: [u8; 32],
    /// The name this device suggests for itself in the invites it mints.
    pub nickname: String,
    pub network: NetworkOptions,
}

/// How a node reaches its peers. Every way is on by default, so that a connection gets through
/// on as many networks as possible without anyone configuring anything.
#[derive(Debug, Clone)]
pub struct NetworkOptions {
    /// Self-hosted relay URLs; empty means the default public relays.
    pub relays: Vec<String>,
    /// With `relays` set, keep n0's public relays next to them so a dead self-hosted relay does not
    /// cut every relayed connection.
    pub relay_fallback_n0: bool,
    /// HTTP(S) proxy for the relay connections.
    pub proxy: proxy::ProxySetting,
    /// Finding peers on the local network (mDNS), which works with no internet at all.
    pub lan: LanDiscovery,
    /// A fixed UDP port for IPv4 (to forward on a router); `None` for a random one.
    pub port: Option<u16>,
}

impl NetworkOptions {
    /// From a `[network]` section; `lan_default` is the role's local-discovery default.
    pub fn from_config(net: &ar_core::config::NetworkSection, lan_default: LanDiscovery) -> Result<Self> {
        Ok(Self {
            relays: net.relays.clone(),
            relay_fallback_n0: net.relay_fallback_n0.unwrap_or(true),
            proxy: proxy::ProxySetting::parse(net.proxy.as_deref()),
            lan: LanDiscovery::parse(net.lan_discovery.as_deref(), lan_default)?,
            port: net.port.filter(|&p| p != 0),
        })
    }
}

/// Local-network (mDNS) discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanDiscovery {
    /// Find peers on the link and announce this device to it (its id and addresses, about once a
    /// second, to every device on the link).
    On,
    /// Find peers without announcing this device.
    Resolve,
    Off,
}

impl LanDiscovery {
    /// Reads `[network] lan_discovery`; absent means `default`.
    pub fn parse(value: Option<&str>, default: Self) -> Result<Self> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") => Ok(default),
            Some("on") => Ok(Self::On),
            Some("resolve") => Ok(Self::Resolve),
            Some("off") => Ok(Self::Off),
            Some(other) => bail!("[network] lan_discovery = {other:?}: expected \"on\", \"resolve\" or \"off\""),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Resolve => "resolve",
            Self::Off => "off",
        }
    }
}

/// n0's public relays, as relay URLs.
fn n0_relay_urls() -> Vec<String> {
    use iroh::defaults::prod::{AP_RELAY_HOSTNAME, EU_RELAY_HOSTNAME, NA_EAST_RELAY_HOSTNAME, NA_WEST_RELAY_HOSTNAME};
    [NA_EAST_RELAY_HOSTNAME, NA_WEST_RELAY_HOSTNAME, EU_RELAY_HOSTNAME, AP_RELAY_HOSTNAME]
        .iter()
        .map(|h| format!("https://{h}"))
        .collect()
}

/// A paired peer as seen by this node.
#[derive(Debug, Clone, Serialize)]
pub struct Peer {
    /// Local name for the peer (unique on this node).
    pub name: String,
    /// The peer's device principal, `eid:<hex>`.
    pub principal: Option<String>,
    /// The peer's person principal, `b64u:<...>`, when it proved one at pairing.
    pub user_id: Option<String>,
    /// Highest level this node grants the peer, if any.
    pub level: Option<Level>,
    /// This node refuses the peer's device (revoked) although the pairing is kept.
    #[serde(default)]
    pub revoked: bool,
}

impl Peer {
    fn has_principal(&self, p: &str) -> bool {
        self.principal.as_deref() == Some(p) || self.user_id.as_deref() == Some(p)
    }
}

/// A device this node refuses, paired or not: revoking keeps the pairing, and a revoked device
/// whose pairing was removed afterwards stays refused until it is unrevoked by its device id.
#[derive(Debug, Clone, Serialize)]
pub struct RevokedDevice {
    /// The device principal, `eid:<hex>`.
    pub principal: String,
    /// This node's name for it while it is still paired.
    pub name: Option<String>,
    pub reason: Option<String>,
}

/// A freshly minted pairing invite.
#[derive(Debug, Clone, Serialize)]
pub struct Invite {
    /// The line to hand to the controller (copy/paste or QR code).
    pub line: String,
    pub expires_at: u64,
    pub level: Level,
}

/// Result of redeeming an invite.
#[derive(Debug, Clone, Serialize)]
pub struct Paired {
    /// Local name of the new peer.
    pub peer: String,
    /// Safety code: the same words must appear on both devices.
    pub sas: String,
    /// Level the invite grants this device on the peer.
    pub level: Option<Level>,
}

/// A controller that just redeemed one of this node's invites, as the inviting side sees it.
#[derive(Debug, Clone, Serialize)]
pub struct PairingEvent {
    /// This node's local name for the new peer.
    pub peer: String,
    /// Safety code: the redeemer was shown the same words; a mismatch means the ticket was
    /// intercepted and the new peer must be removed.
    pub sas: String,
    /// Level this node grants the new peer.
    pub level: Option<Level>,
    /// The peer's stable principal (`b64u:` or `eid:`).
    pub principal: Option<String>,
}

/// This node's own network posture: the first question when peers cannot reach it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStatus {
    /// A home-relay connection is established.
    pub online: bool,
    /// The connected home relay (scheme, host and port only).
    pub home_relay: Option<String>,
    /// Every known relay and whether it is connected.
    pub relays: Vec<RelayState>,
    /// This node's direct socket addresses.
    pub direct_addrs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayState {
    pub url: String,
    pub connected: bool,
}

/// What this node's network allows for direct (P2P) connections, from the transport's latest
/// network report. Addresses are deliberately left out: this travels to local clients and agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NatReport {
    /// UDP gets out and back over IPv4.
    pub udp_v4: bool,
    /// UDP gets out and back over IPv6.
    pub udp_v6: bool,
    /// The IPv4 NAT gives each destination its own public port ("symmetric"); hole punching
    /// through it rarely works.
    pub symmetric_nat: Option<bool>,
    /// A captive portal (a sign-in page of a hotel or café network) seems to intercept traffic.
    pub captive_portal: Option<bool>,
}

impl NatReport {
    /// The likely reason direct connections fail from this side, if any: `"udp_blocked"`,
    /// `"symmetric_nat"` or `"captive_portal"`.
    pub fn obstacle(&self) -> Option<&'static str> {
        if self.captive_portal == Some(true) {
            Some("captive_portal")
        } else if !self.udp_v4 && !self.udp_v6 {
            Some("udp_blocked")
        } else if self.symmetric_nat == Some(true) && !self.udp_v6 {
            Some("symmetric_nat")
        } else {
            None
        }
    }
}

/// Whether a connection's traffic goes direct or through a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Direct,
    Relay,
    Unknown,
}

/// The route a connection's traffic takes right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Route {
    pub path: PathKind,
    /// The relay carrying the traffic (`scheme://host[:port]`), when relayed and known.
    pub relay: Option<String>,
}

impl Route {
    /// The traffic goes through one of n0's public relays: the default when no `[network] relays`
    /// are configured, rate-limited and meant by n0 for development rather than production.
    pub fn via_n0(&self) -> bool {
        self.path == PathKind::Relay && self.relay.as_deref().is_some_and(is_n0_relay)
    }
}

/// Whether `relay` (a relay URL or host name) is one of n0's public relays.
pub fn is_n0_relay(relay: &str) -> bool {
    let host = relay_host(relay);
    n0_relay_urls().iter().any(|u| relay_host(u) == host)
}

/// The host of a relay URL (or host name), lowercased, without the trailing root dot.
fn relay_host(relay: &str) -> String {
    let host = relay.split_once("://").map_or(relay, |(_, rest)| rest);
    host.split(['/', ':']).next().unwrap_or_default().trim_end_matches('.').to_ascii_lowercase()
}

/// A running node.
#[derive(Clone)]
pub struct Mesh {
    node: Arc<Node>,
}

impl Mesh {
    /// Starts the node. Requires a multi-thread tokio runtime.
    pub async fn start(opts: MeshOptions) -> Result<Self> {
        write_node_config(&opts)?;
        let node = NodeBuilder::new(&opts.root)
            .device_key(SigningKey::from_bytes(&opts.device_key))
            .start()
            .await
            .map_err(|e| anyhow!("start transport node: {e}"))?;
        let mesh = Self { node: Arc::new(node) };
        Ok(mesh)
    }

    /// This device's principal, `eid:<hex>`.
    pub fn principal(&self) -> String {
        format!("eid:{}", self.node.endpoint_id())
    }

    /// Short form of the device id for display.
    pub fn short_id(&self) -> String {
        self.node.endpoint_id().fmt_short().to_string()
    }

    async fn control(&self) -> Result<ControlClient> {
        self.node.control().await.map_err(|e| anyhow!("transport control: {e}"))
    }

    /// Registers the three level marker services (idempotent). Hosts call this once at start.
    ///
    /// Each marker is backed by a local endpoint nothing listens on, so a stock mcpmesh client that
    /// tries to open one fails closed; ai-remote's own traffic never uses them.
    pub async fn ensure_level_services(&self) -> Result<()> {
        let mut c = self.control().await?;
        let status = c.status().await.map_err(|e| anyhow!("status: {e}"))?;
        for level in Level::ALL {
            let name = service_name(level);
            if status.services.iter().any(|s| s.name == name) {
                continue;
            }
            #[cfg(windows)]
            let path = format!(r"\\.\pipe\ai-remote-marker-{level}-unused");
            #[cfg(not(windows))]
            let path = format!("/nonexistent/ai-remote-marker-{level}.sock");
            c.register_service(&name, BackendSpec::Socket { path }, vec![])
                .await
                .map_err(|e| anyhow!("register {name}: {e}"))?;
        }
        Ok(())
    }

    /// Mints a single-use invite granting `level`. `peer_name` is this host's local name for
    /// whoever redeems it.
    pub async fn invite(&self, level: Level, peer_name: Option<String>) -> Result<Invite> {
        let mut c = self.control().await?;
        let r = c
            .invite_named(vec![service_name(level)], None, None, peer_name.map(|n| sanitize_name(&n)))
            .await
            .map_err(|e| anyhow!("mint invite: {e}"))?;
        Ok(Invite { line: r.invite_line, expires_at: r.expires_at_epoch, level })
    }

    /// Redeems an invite from a host. `name` is this device's local name for the host.
    pub async fn pair(&self, line: &str, name: Option<&str>) -> Result<Paired> {
        let mut c = self.control().await?;
        let r = c.pair_as(line.trim(), name.map(sanitize_name)).await.map_err(|e| anyhow!("pair: {e}"))?;
        let level = r
            .services
            .iter()
            .filter_map(|s| s.strip_prefix(SERVICE_PREFIX).and_then(|l| l.parse::<Level>().ok()))
            .max();
        Ok(Paired { peer: r.peer_nickname, sas: r.sas_code, level })
    }

    /// All paired peers with the level this node grants each.
    pub async fn peers(&self) -> Result<Vec<Peer>> {
        let mut c = self.control().await?;
        let status = c.status().await.map_err(|e| anyhow!("status: {e}"))?;
        Ok(peers_from_status(&status))
    }

    /// This node's relay connection and direct addresses (`None` before the node reports them).
    pub async fn network(&self) -> Result<Option<NetworkStatus>> {
        let mut c = self.control().await?;
        let status = c.status().await.map_err(|e| anyhow!("status: {e}"))?;
        Ok(status.self_network.map(|n| NetworkStatus {
            online: n.online,
            home_relay: n.home_relay,
            relays: n.relays.into_iter().map(|r| RelayState { url: r.url, connected: r.connected }).collect(),
            direct_addrs: n.direct_addrs,
        }))
    }

    /// Starts watching for controllers that redeem this node's invites. Start it before handing
    /// out a ticket: a pairing that completes before the watch starts is not reported.
    pub async fn watch_pairings(&self) -> Result<PairingWatch> {
        let mut control = self.control().await?;
        let status = control.status().await.map_err(|e| anyhow!("status: {e}"))?;
        let announced = status.recent_pairings.iter().map(pairing_key).collect();
        let events =
            self.control().await?.subscribe().await.map_err(|e| anyhow!("subscribe to transport events: {e}"))?;
        Ok(PairingWatch { events, control, announced })
    }

    /// The paired peer behind an authenticated connection, if any.
    pub async fn peer_for(&self, remote: &iroh::EndpointId) -> Result<Option<Peer>> {
        let principal = format!("eid:{remote}");
        Ok(self.peers().await?.into_iter().find(|p| p.has_principal(&principal)))
    }

    /// Finds a peer by local name, `eid:`/`b64u:` principal, or a unique prefix of either.
    pub async fn find_peer(&self, query: &str) -> Result<Peer> {
        let peers = self.peers().await?;
        if let Some(p) = peers.iter().find(|p| p.name == query || p.has_principal(query)) {
            return Ok(p.clone());
        }
        let matches: Vec<&Peer> = peers
            .iter()
            .filter(|p| {
                p.name.starts_with(query)
                    || p.principal.as_deref().is_some_and(|x| x.starts_with(query))
                    || p.principal.as_deref().is_some_and(|x| x.trim_start_matches("eid:").starts_with(query))
            })
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no paired peer matches '{query}'"),
            _ => bail!("'{query}' matches several peers; use the full name"),
        }
    }

    /// Sets (or with `None`, removes) the level this node grants `peer`.
    pub async fn set_level(&self, peer: &Peer, level: Option<Level>) -> Result<()> {
        let principal =
            peer.user_id.clone().or_else(|| peer.principal.clone()).context("peer has no stable principal")?;
        let mut c = self.control().await?;
        for l in Level::ALL {
            let name = service_name(l);
            if Some(l) == level {
                c.service_allow_grant(&name, &principal).await.map_err(|e| anyhow!("grant: {e}"))?;
            } else {
                // Drop both principal forms so an old grant cannot linger.
                for p in [peer.user_id.as_deref(), peer.principal.as_deref()].into_iter().flatten() {
                    c.service_allow_revoke(&name, p).await.map_err(|e| anyhow!("revoke grant: {e}"))?;
                }
            }
        }
        Ok(())
    }

    /// Refuses a peer's device from now on and cuts its live connections.
    pub async fn revoke(&self, peer: &Peer, reason: Option<String>) -> Result<()> {
        let principal = peer.principal.clone().context("peer has no device principal")?;
        let mut c = self.control().await?;
        c.peer_revoke(principal, reason).await.map_err(|e| anyhow!("revoke: {e}"))?;
        Ok(())
    }

    /// Lifts the revocation of a device (`eid:` principal), paired or not.
    pub async fn unrevoke(&self, principal: &str) -> Result<()> {
        let mut c = self.control().await?;
        c.peer_unrevoke(principal.to_string()).await.map_err(|e| anyhow!("unrevoke: {e}"))?;
        Ok(())
    }

    /// Every device this node refuses, including revoked devices whose pairing is gone.
    pub async fn revoked_devices(&self) -> Result<Vec<RevokedDevice>> {
        let mut c = self.control().await?;
        let status = c.status().await.map_err(|e| anyhow!("status: {e}"))?;
        Ok(status
            .revoked
            .into_iter()
            .map(|r| RevokedDevice { principal: r.principal, name: r.nickname, reason: r.reason })
            .collect())
    }

    /// Finds a refused device by name, `eid:` principal or a unique prefix of its id.
    pub async fn find_revoked(&self, query: &str) -> Result<Option<RevokedDevice>> {
        let devices = self.revoked_devices().await?;
        if let Some(d) = devices.iter().find(|d| d.principal == query || d.name.as_deref() == Some(query)) {
            return Ok(Some(d.clone()));
        }
        let id = query.trim_start_matches("eid:");
        let matches: Vec<&RevokedDevice> = devices
            .iter()
            .filter(|d| !id.is_empty() && d.principal.trim_start_matches("eid:").starts_with(id))
            .collect();
        match matches.as_slice() {
            [one] => Ok(Some((*one).clone())),
            [] => Ok(None),
            _ => bail!("'{query}' matches several revoked devices; use the full device id"),
        }
    }

    /// Forgets a peer entirely (its pairing and grants). The device can come back only by
    /// redeeming a new invite; use [`revoke`](Self::revoke) to refuse even that. The transport cuts
    /// the peer's live connections with its generic "access revoked" reason: close them first with
    /// a reason of your own if the peer should hear something else.
    pub async fn forget(&self, peer: &Peer) -> Result<()> {
        let mut c = self.control().await?;
        c.peer_remove(&peer.name).await.map_err(|e| anyhow!("remove peer: {e}"))?;
        Ok(())
    }

    /// Serves `alpn` with `handler`. Only paired, non-revoked peers ever reach the handler.
    pub fn accept(&self, alpn: &[u8], handler: Arc<dyn iroh::protocol::DynProtocolHandler>) -> Result<()> {
        self.node.accept_protocol(alpn, handler)
    }

    /// Dials a paired peer (by local name or principal) on `alpn`.
    pub async fn dial(&self, peer: &str, alpn: &[u8]) -> Result<iroh::endpoint::Connection> {
        self.node.connect_protocol(peer, alpn).await
    }

    /// What this node's network allows for direct connections (`None` until the first network
    /// report, a few seconds after start).
    pub fn nat(&self) -> Option<NatReport> {
        let r = self.node.net_report()?;
        Some(NatReport {
            udp_v4: r.udp_v4,
            udp_v6: r.udp_v6,
            symmetric_nat: r.mapping_varies_by_dest_ipv4,
            captive_portal: r.captive_portal,
        })
    }

    /// Measures whether a connection's traffic is direct or relayed (takes up to ~0.75 s).
    pub async fn path(conn: &iroh::endpoint::Connection) -> PathKind {
        Self::route(conn).await.path
    }

    /// Like [`path`](Self::path), plus the relay in use when relayed.
    pub async fn route(conn: &iroh::endpoint::Connection) -> Route {
        match Node::connection_path(conn).await {
            PeerPath::Direct => Route { path: PathKind::Direct, relay: None },
            PeerPath::Relay { url } => Route { path: PathKind::Relay, relay: url },
            _ => Route { path: PathKind::Unknown, relay: None },
        }
    }

    /// Stops serving and closes the endpoint gracefully.
    pub async fn shutdown(self) {
        match Arc::try_unwrap(self.node) {
            Ok(node) => node.shutdown().await,
            Err(_) => tracing::warn!("transport node still shared at shutdown; not closed gracefully"),
        }
    }
}

fn peers_from_status(status: &StatusResult) -> Vec<Peer> {
    let mut peers: Vec<Peer> = status
        .peers
        .iter()
        .map(|p| Peer {
            name: p.name.clone(),
            principal: p.principal.clone(),
            user_id: p.user_id.clone(),
            level: None,
            revoked: p.principal.as_ref().is_some_and(|eid| status.revoked.iter().any(|r| &r.principal == eid)),
        })
        .collect();
    for peer in &mut peers {
        for level in Level::ALL {
            let name = service_name(level);
            let granted = status
                .services
                .iter()
                .filter(|s| s.name == name)
                .any(|s| s.allow.iter().any(|a| peer.has_principal(a)));
            if granted {
                peer.level = Some(peer.level.map_or(level, |l| l.max(level)));
            }
        }
    }
    peers
}

/// Identity of one completed ceremony in the transport's recent-pairings ring.
fn pairing_key(p: &RecentPairing) -> (String, String, u64) {
    (p.peer_nickname.clone(), p.sas_code.clone(), p.paired_at_epoch)
}

/// Live feed of completed pairings, from [`Mesh::watch_pairings`].
///
/// The transport reports a pairing as a `trust`/`pair` event and parks the inviter's side of the
/// safety code in an in-memory ring that `status` exposes. The event is emitted a moment before
/// the ring entry is written, so the watch polls `status` briefly after each event.
pub struct PairingWatch {
    events: StreamSubscription,
    control: ControlClient,
    announced: HashSet<(String, String, u64)>,
}

impl PairingWatch {
    /// The next completed pairing, or `None` when the node stops.
    pub async fn next(&mut self) -> Result<Option<PairingEvent>> {
        loop {
            let frame = match self.events.next().await {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(None),
                // A frame this build does not model; nothing to do with it.
                Err(ClientError::Malformed(_)) => continue,
                Err(e) => return Err(anyhow!("transport event stream: {e}")),
            };
            let wanted = match frame {
                StreamFrame::Event { record }
                    if record.kind == AuditKind::Trust && record.event.as_deref() == Some("pair") =>
                {
                    record.target
                }
                // Events were dropped; one of them may have been a pairing.
                StreamFrame::Lagged { .. } => None,
                _ => continue,
            };
            if let Some(event) = self.find_new(wanted.as_deref()).await? {
                return Ok(Some(event));
            }
        }
    }

    /// Polls `status` for a pairing not announced yet (of `peer`, when the event named one).
    async fn find_new(&mut self, peer: Option<&str>) -> Result<Option<PairingEvent>> {
        // Only a targeted event is worth waiting for; a lag notice gets one look.
        let attempts = if peer.is_some() { 20 } else { 1 };
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let status = self.control.status().await.map_err(|e| anyhow!("status: {e}"))?;
            let fresh = status.recent_pairings.iter().find(|p| {
                !p.self_enroll && peer.is_none_or(|n| p.peer_nickname == n) && !self.announced.contains(&pairing_key(p))
            });
            if let Some(p) = fresh {
                self.announced.insert(pairing_key(p));
                let found = peers_from_status(&status).into_iter().find(|x| x.name == p.peer_nickname);
                return Ok(Some(PairingEvent {
                    peer: p.peer_nickname.clone(),
                    sas: p.sas_code.clone(),
                    level: found.as_ref().and_then(|x| x.level),
                    principal: found.and_then(|x| x.user_id.or(x.principal)),
                }));
            }
        }
        // This node redeeming another node's invite also emits `pair`, without a ring entry.
        Ok(None)
    }
}

/// Nicknames become part of peers' local names; keep them short and shell-friendly.
pub fn sanitize_name(name: &str) -> String {
    let mut out: String =
        name.trim().chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    let out: String = out.chars().take(40).collect();
    if out.is_empty() { "device".to_string() } else { out }
}

/// Writes our settings into the node's config file (read at start), keeping every other key the
/// node itself persists there (level grants live in its `[services.*]` tables).
fn write_node_config(opts: &MeshOptions) -> Result<()> {
    let dir = opts.root.join("config");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("config.toml");
    let mut doc: toml::Table = match std::fs::read_to_string(&path) {
        Ok(text) => text.parse().with_context(|| format!("parse {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let identity = table_entry(&mut doc, "identity");
    identity.insert("nickname".into(), sanitize_name(&opts.nickname).into());
    let network = table_entry(&mut doc, "network");
    let net = &opts.network;
    if net.relays.is_empty() {
        network.remove("relay_mode");
        network.remove("relay_urls");
    } else {
        let mut urls = net.relays.clone();
        if net.relay_fallback_n0 {
            urls.extend(
                n0_relay_urls().into_iter().filter(|u| !net.relays.iter().any(|r| relay_host(r) == relay_host(u))),
            );
        }
        network.insert("relay_mode".into(), "custom".into());
        network.insert(
            "relay_urls".into(),
            toml::Value::Array(urls.iter().map(|u| toml::Value::from(u.as_str())).collect()),
        );
    }
    match net.proxy.resolve() {
        Some((url, source)) => {
            tracing::info!(proxy = %url, source, "relay connections go through an HTTP proxy");
            network.insert("proxy_url".into(), url.into());
        }
        None => {
            network.remove("proxy_url");
        }
    }
    network.insert("local_discovery".into(), net.lan.as_str().into());
    match net.port {
        Some(port) => network.insert("bind_port".into(), i64::from(port).into()),
        None => network.remove("bind_port"),
    };
    std::fs::write(&path, toml::to_string(&doc)?).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn table_entry<'a>(doc: &'a mut toml::Table, key: &str) -> &'a mut toml::Table {
    let entry = doc.entry(key.to_string()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::Table::new());
    }
    entry.as_table_mut().expect("just ensured a table")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("Office PC (2)"), "office-pc-2");
        assert_eq!(sanitize_name("  "), "device");
        assert_eq!(sanitize_name("DESKTOP-AB12"), "desktop-ab12");
    }

    #[test]
    fn writes_every_way_to_reach_peers_into_the_node_config() {
        let dir = tempfile::tempdir().unwrap();
        let write = |relays: &[&str], relay_fallback_n0: bool, proxy: proxy::ProxySetting, lan: LanDiscovery| {
            write_node_config(&MeshOptions {
                root: dir.path().into(),
                device_key: [7; 32],
                nickname: "pc".into(),
                network: NetworkOptions {
                    relays: relays.iter().map(|r| r.to_string()).collect(),
                    relay_fallback_n0,
                    proxy,
                    lan,
                    port: (lan == LanDiscovery::Resolve).then_some(41641),
                },
            })
            .unwrap();
            let doc: toml::Table =
                std::fs::read_to_string(dir.path().join("config/config.toml")).unwrap().parse().unwrap();
            doc["network"].as_table().unwrap().clone()
        };
        let urls = |net: &toml::Table| net["relay_urls"].as_array().unwrap().len();

        let net = write(&[], true, proxy::ProxySetting::Off, LanDiscovery::On);
        assert!(net.get("relay_urls").is_none() && net.get("proxy_url").is_none());
        assert_eq!(net["local_discovery"].as_str(), Some("on"));

        let proxy = proxy::ProxySetting::Url("proxy.corp:3128".into());
        let net = write(&["https://relay.example.org"], true, proxy, LanDiscovery::Resolve);
        assert_eq!(net["relay_mode"].as_str(), Some("custom"));
        assert_eq!(urls(&net), 5, "the self-hosted relay plus n0's four as a fallback");
        assert_eq!(net["proxy_url"].as_str(), Some("http://proxy.corp:3128"));
        assert_eq!(net["local_discovery"].as_str(), Some("resolve"));
        assert_eq!(net["bind_port"].as_integer(), Some(41641));

        assert_eq!(urls(&write(&["https://relay.example.org"], false, proxy::ProxySetting::Off, LanDiscovery::Off)), 1);
        let with_n0 = write(&["https://euc1-1.relay.n0.iroh.link"], true, proxy::ProxySetting::Off, LanDiscovery::Off);
        assert_eq!(urls(&with_n0), 4, "an n0 relay already listed is not added twice");
        assert!(with_n0.get("proxy_url").is_none(), "turning the proxy off removes the old one");
        assert!(with_n0.get("bind_port").is_none(), "dropping the port removes the old one");
    }

    #[test]
    fn names_what_keeps_direct_connections_from_working() {
        let open = NatReport { udp_v4: true, udp_v6: false, symmetric_nat: Some(false), captive_portal: None };
        assert_eq!(open.obstacle(), None);
        assert_eq!(NatReport { udp_v4: false, udp_v6: false, ..open }.obstacle(), Some("udp_blocked"));
        assert_eq!(NatReport { symmetric_nat: Some(true), ..open }.obstacle(), Some("symmetric_nat"));
        assert_eq!(NatReport { symmetric_nat: Some(true), udp_v6: true, ..open }.obstacle(), None, "IPv6 needs no NAT");
        assert_eq!(NatReport { captive_portal: Some(true), ..open }.obstacle(), Some("captive_portal"));
    }

    #[test]
    fn reads_the_lan_discovery_setting() {
        assert_eq!(LanDiscovery::parse(None, LanDiscovery::On).unwrap(), LanDiscovery::On);
        assert_eq!(LanDiscovery::parse(Some(" Resolve "), LanDiscovery::On).unwrap(), LanDiscovery::Resolve);
        assert_eq!(LanDiscovery::parse(Some("off"), LanDiscovery::On).unwrap(), LanDiscovery::Off);
        assert!(LanDiscovery::parse(Some("of"), LanDiscovery::On).is_err());
    }

    #[test]
    fn tells_n0_relays_from_self_hosted_ones() {
        assert!(is_n0_relay("https://use1-1.relay.n0.iroh.link."));
        assert!(is_n0_relay("https://APS1-1.relay.n0.iroh.link:443"));
        assert!(is_n0_relay("euc1-1.relay.n0.iroh.link"));
        assert!(!is_n0_relay("https://relay.example.org"));
        assert!(!is_n0_relay("https://use1-1.relay.n0.iroh.link.evil.example"));
        let relayed = |url: &str| Route { path: PathKind::Relay, relay: Some(url.into()) };
        assert!(relayed("https://usw1-1.relay.n0.iroh.link.").via_n0());
        assert!(!relayed("https://relay.example.org").via_n0());
        assert!(!Route { path: PathKind::Direct, relay: None }.via_n0());
    }
}
