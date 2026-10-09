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

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Level;
use mcpmesh_local_api::{BackendSpec, ControlClient};
use mcpmesh_node::{Node, NodeBuilder, mcpmesh_trust::ed25519_dalek::SigningKey};
use serde::Serialize;

pub use mcpmesh_local_api::PeerPath;
pub use mcpmesh_node::iroh;

/// ALPN of an MCP session: the controller is the MCP client, the host the MCP server.
pub const ALPN_MCP: &[u8] = b"app/ai-remote/mcp/1";

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
    /// Self-hosted relay URLs; empty means the default public relays.
    pub relays: Vec<String>,
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
}

impl Peer {
    fn has_principal(&self, p: &str) -> bool {
        self.principal.as_deref() == Some(p) || self.user_id.as_deref() == Some(p)
    }
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

/// Whether a connection's traffic goes direct or through a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Direct,
    Relay,
    Unknown,
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
            c.register_service(&name, BackendSpec::Socket { path: path.into() }, vec![])
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
        let r = c
            .pair_as(line.trim(), name.map(sanitize_name))
            .await
            .map_err(|e| anyhow!("pair: {e}"))?;
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
        let mut peers: Vec<Peer> = status
            .peers
            .iter()
            .map(|p| Peer {
                name: p.name.clone(),
                principal: p.principal.clone(),
                user_id: p.user_id.clone(),
                level: None,
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
        Ok(peers)
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
        let principal = peer
            .user_id
            .clone()
            .or_else(|| peer.principal.clone())
            .context("peer has no stable principal")?;
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

    /// Lifts a revocation.
    pub async fn unrevoke(&self, peer: &Peer) -> Result<()> {
        let principal = peer.principal.clone().context("peer has no device principal")?;
        let mut c = self.control().await?;
        c.peer_unrevoke(principal).await.map_err(|e| anyhow!("unrevoke: {e}"))?;
        Ok(())
    }

    /// Forgets a peer entirely (its pairing and grants). Use [`revoke`](Self::revoke) to also
    /// refuse it if it comes back.
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

    /// Measures whether a connection's traffic is direct or relayed (takes up to ~0.75 s).
    pub async fn path(conn: &iroh::endpoint::Connection) -> PathKind {
        match Node::connection_path(conn).await {
            PeerPath::Direct => PathKind::Direct,
            PeerPath::Relay { .. } => PathKind::Relay,
            _ => PathKind::Unknown,
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

/// Nicknames become part of peers' local names; keep them short and shell-friendly.
pub fn sanitize_name(name: &str) -> String {
    let mut out: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
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
    if opts.relays.is_empty() {
        network.remove("relay_mode");
        network.remove("relay_urls");
    } else {
        network.insert("relay_mode".into(), "custom".into());
        network.insert(
            "relay_urls".into(),
            toml::Value::Array(opts.relays.iter().map(|u| toml::Value::from(u.as_str())).collect()),
        );
    }
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
}
