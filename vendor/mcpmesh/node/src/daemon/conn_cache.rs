//! One client connection per remote for MCP sessions (#215).
//!
//! The accept side has multiplexed sessions as separate bi-streams on one connection since 0.1.0
//! (`run_mesh_connection` loops `accept_bi`). The dial side opened one connection per
//! `open_session` out of nothing but convenience, and that is the K ≥ 2 that iroh#4390 turns into
//! an unbounded `pending_open_paths` deque. This cache makes a second session to the same peer a
//! second bi-stream on the connection the first one is already using.
//!
//! **Weak, not strong.** Each entry is a [`WeakConnectionHandle`]: the connection lives exactly as
//! long as some session's streams hold it and dies with the last one, as before. No idle connection
//! is kept warm, so no new keepalive or idle-timeout semantics appear. A stale entry — one that no
//! longer upgrades, or whose `close_reason()` is set — is skipped and replaced by a fresh dial.
//!
//! **Keyed by peer endpoint id, for `ALPN_MCP` only.** Nothing else is ever inserted: app-protocol
//! connections negotiate their own ALPN at handshake and can never share. The reachability probe
//! does not use this cache: it dials `mcpmesh/ping/1` exactly as before, so a peer that something
//! polls briefly has a second connection once per TTL.
//!
//! **Single-flight dials, per device.** Two sessions opened at the same instant to a peer with no
//! live connection would each dial. A caller that finds nothing inserts a `Dialing` slot for every
//! device it is about to dial — one for a single-target dial, one per candidate for a racing dial —
//! and leads; a caller that finds any of its candidates `Dialing` waits for that dial and looks
//! again. So a `b64u:` race and an `eid:` dial to one of the same devices never run side by side. The
//! std mutex is held only for the map lookup/insert — never across an await. A leader that fails, or
//! is cancelled mid-dial, clears its slots on drop so waiters dial for themselves.
//!
//! **What is NOT one connection.** A session with its own `idle_timeout_secs` dials its own
//! connection (the knob is per connection). A connection that has run out of stream credit — the
//! peer's `max_concurrent_bidi_streams`, 100 by default — is not stale, but the next session cannot
//! wait on it indefinitely, so after [`REUSE_OPEN_TIMEOUT`] it dials a connection of its own that is
//! not cached and dies with that session. And a peer that restarts while a dial is in flight can
//! leave the loser of that race alive for its session.
//!
//! **Authorization is the ACCEPTING side's, per stream.** Since #222 `run_session` resolves the
//! caller's principals and the allow list per bi-stream, so a second session on a shared connection
//! is admitted or refused exactly as a fresh connection would be. A peer still on mcpmesh ≤ 0.53.2
//! resolves the caller's identity once per connection, so against such a peer a stream opened after
//! the caller lost a principal is admitted with the principal it had when the connection was
//! accepted — and sharing stretches that window to the shared connection's lifetime.
//!
//! **Revocation.** Three guards, in the order a dial meets them. The dial paths ask `dial_refused`
//! before they reach this cache (`refuse_if_revoked`, and `hinted_addrs` for a race). The cache asks
//! again, through the caller's `refused` predicate, before it hands out a cached connection: a
//! session that waited on another caller's dial passed the first check before that wait, and a
//! revocation written without a revoke verb closes nothing. And the endpoint hooks (#229) close
//! every registered connection to a newly refused device on the revoke verbs and roster installs; a
//! connection closed that way reads as dead here.
//!
//! The hand-out check covers REUSE only. A session that dials for itself after waiting — its leader
//! failed, or the shared connection had no stream credit — does not ask again; on a booted node
//! #229's `before_connect` veto refuses that dial, and on an endpoint without the hooks (a mesh a
//! test assembles by hand) nothing does.
//!
//! [`WeakConnectionHandle`]: iroh::endpoint::WeakConnectionHandle

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use mcpmesh_net::SessionTransport;
use mcpmesh_net::framing::MAX_FRAME_BYTES;

type Connection = iroh::endpoint::Connection;

/// What a waiter reads off a `Dialing` slot: `Some` once the leader FAILED, carrying why.
type LeadOutcome = tokio::sync::watch::Receiver<Option<Arc<str>>>;

/// How long a session waits for stream credit on a shared connection before dialling its own.
///
/// Opening a stream needs no round trip when credit is available, so on a healthy connection this
/// resolves at once; it only ever elapses on a connection whose peer has admitted as many concurrent
/// streams as it allows. One second keeps a session past that limit from paying `DIAL_TIMEOUT` and
/// then failing with a message about dialling.
pub(crate) const REUSE_OPEN_TIMEOUT: Duration = Duration::from_secs(1);

enum Slot {
    /// A connection some session may still be holding open.
    Live(iroh::endpoint::WeakConnectionHandle),
    /// A dial in flight; waiters block on the receiver until the leader settles or gives up.
    Dialing(LeadOutcome),
}

/// The per-peer client-connection cache. `Clone` shares the map (the dial lead needs a handle).
#[derive(Clone, Default)]
pub(crate) struct McpConnCache {
    inner: Arc<Mutex<HashMap<[u8; 32], Slot>>>,
}

/// What [`McpConnCache::session_on`] produced.
pub(crate) enum Opened {
    /// A bi-stream on a connection that already existed. No new connection, no new watcher.
    Reused(SessionTransport),
    /// A freshly dialled connection, handed back so the caller can attach its path watcher.
    Fresh(SessionTransport, Connection),
}

/// How opening a stream on a cached connection went.
enum Stream {
    Open(SessionTransport),
    /// The connection refused a stream — closed under us. Its entry has been forgotten.
    Stale,
    /// No stream credit within [`REUSE_OPEN_TIMEOUT`]. The connection is fine and stays cached.
    Saturated,
}

enum Claim {
    Live([u8; 32], Connection),
    Wait(LeadOutcome),
    Lead(DialLead),
}

impl McpConnCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The map, recovering from a poisoned lock the way `DialLead::drop` does: it is plain data, so
    /// the value behind the poison is still coherent, and a panic in one caller must not turn every
    /// later `open_session` into a panic too.
    fn map(&self) -> MutexGuard<'_, HashMap<[u8; 32], Slot>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The live connection to `peer`, if the cache holds one that still upgrades and is not closed.
    /// A stale entry is dropped on the way out. Test-only: the dial paths go through `claim`, and
    /// the tests read the cache's state through the same `open` rule it uses.
    #[cfg(test)]
    pub(crate) fn live(&self, peer: [u8; 32]) -> Option<Connection> {
        let mut map = self.map();
        match map.get(&peer) {
            Some(Slot::Live(weak)) => match open(weak) {
                Some(conn) => Some(conn),
                None => {
                    map.remove(&peer);
                    None
                }
            },
            _ => None,
        }
    }

    /// Open a bi-stream on a connection this cache handed out, waiting at most
    /// [`REUSE_OPEN_TIMEOUT`] for stream credit. On failure the entry is removed if it still names
    /// THIS connection — a newer connection cached meanwhile is left alone — so a caller retrying
    /// its claim leads a fresh dial rather than looping on a handle that cannot open streams.
    async fn open_on(&self, peer: [u8; 32], conn: &Connection) -> Stream {
        match tokio::time::timeout(REUSE_OPEN_TIMEOUT, conn.open_bi()).await {
            Ok(Ok((send, recv))) => {
                Stream::Open(SessionTransport::new(recv, send, MAX_FRAME_BYTES))
            }
            Ok(Err(e)) => {
                tracing::debug!(%e, "cached mesh connection refused a new stream; dialling fresh");
                let mut map = self.map();
                if let Some(Slot::Live(weak)) = map.get(&peer)
                    && weak
                        .upgrade()
                        .is_some_and(|c| c.stable_id() == conn.stable_id())
                {
                    map.remove(&peer);
                }
                Stream::Stale
            }
            Err(_) => {
                tracing::debug!(
                    "cached mesh connection has no stream credit; this session dials its own"
                );
                Stream::Saturated
            }
        }
    }

    /// One look at `peers`, under one lock: a live connection to any of them wins; else any dial
    /// already in flight to one of them is waited on; else this caller leads a dial to all of them.
    fn claim(&self, peers: &[[u8; 32]]) -> Claim {
        let mut map = self.map();
        for peer in peers {
            if let Some(Slot::Live(weak)) = map.get(peer) {
                match open(weak) {
                    Some(conn) => return Claim::Live(*peer, conn),
                    None => {
                        map.remove(peer);
                    }
                }
            }
        }
        for peer in peers {
            if let Some(Slot::Dialing(rx)) = map.get(peer) {
                return Claim::Wait(rx.clone());
            }
        }
        let (tx, rx) = tokio::sync::watch::channel(None);
        for peer in peers {
            map.insert(*peer, Slot::Dialing(rx.clone()));
        }
        Claim::Lead(DialLead {
            cache: self.clone(),
            peers: peers.to_vec(),
            settled: false,
            tx,
        })
    }

    /// A session to one of `peers`: a new bi-stream on a live connection to any of them if there
    /// is one, else ONE `dial` shared with every other caller that arrives while it is in flight.
    ///
    /// `dial` runs at most once per call: when this caller leads, or when the live connection it
    /// found has no stream credit (that connection stays cached; this one is not). A waiter woken
    /// by a leader that failed looks again and may lead itself. `deadline` bounds the whole thing,
    /// waiting included, and a timeout reports the failure of the dial it was waiting on when one
    /// is known — "timed out" alone would hide that the peer had already refused.
    pub(crate) async fn session_on<F, Fut, R, RFut>(
        &self,
        peers: &[[u8; 32]],
        deadline: Duration,
        refused: R,
        dial: F,
    ) -> Result<Opened>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(SessionTransport, Connection)>>,
        R: Fn([u8; 32]) -> RFut,
        RFut: std::future::Future<Output = Result<bool>>,
    {
        let mut leader_failure: Option<Arc<str>> = None;
        let claimed = self.claim_loop(peers, refused, dial, &mut leader_failure);
        match tokio::time::timeout(deadline, claimed).await {
            Ok(opened) => opened,
            Err(_) => match leader_failure {
                Some(why) => anyhow::bail!(
                    "dial timed out after {deadline:?}; the dial this session was waiting on had \
                     already failed: {why}"
                ),
                None => anyhow::bail!("dial timed out after {deadline:?}"),
            },
        }
    }

    async fn claim_loop<F, Fut, R, RFut>(
        &self,
        peers: &[[u8; 32]],
        refused: R,
        dial: F,
        leader_failure: &mut Option<Arc<str>>,
    ) -> Result<Opened>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(SessionTransport, Connection)>>,
        R: Fn([u8; 32]) -> RFut,
        RFut: std::future::Future<Output = Result<bool>>,
    {
        let mut dial = Some(dial);
        let mut peers = peers.to_vec();
        loop {
            match self.claim(&peers) {
                // A connection is only as reusable as its device is dialable (#215 with #229). The
                // caller checked before any waiting; this is the check at the moment of handing one
                // out. A refused device leaves the candidate set and the claim is retried, so the
                // rest of a person's devices stay reachable; with none left, the session is refused.
                // The connection itself is left for the revoke paths to close — other sessions on
                // it are theirs to end, not this caller's.
                //
                // A check that could not run refuses this reuse (fail closed) and says so, rather than
                // calling a device it knows nothing about revoked.
                Claim::Live(peer, conn) => {
                    match refused(peer).await {
                        Ok(false) => {}
                        Ok(true) => {
                            peers.retain(|p| *p != peer);
                            anyhow::ensure!(
                                !peers.is_empty(),
                                "the device this session would reuse a connection to is REVOKED \
                                 on this node"
                            );
                            continue;
                        }
                        Err(e) => {
                            return Err(e.context(
                                "the revocation check failed, so this session does not reuse the \
                                 cached connection",
                            ));
                        }
                    }
                    match self.open_on(peer, &conn).await {
                        Stream::Open(t) => return Ok(Opened::Reused(t)),
                        // The entry was forgotten; the next claim leads.
                        Stream::Stale => {}
                        Stream::Saturated => {
                            let dial = dial.take().expect("a caller dials at most once");
                            let (transport, conn) = dial().await?;
                            return Ok(Opened::Fresh(transport, conn));
                        }
                    }
                }
                Claim::Wait(mut rx) => {
                    // Ok: the leader failed and said why. Err: it settled, or was cancelled.
                    // Either way, look again.
                    if rx.changed().await.is_ok() {
                        leader_failure.clone_from(&rx.borrow());
                    }
                }
                Claim::Lead(lead) => {
                    let dial = dial.take().expect("a caller dials at most once");
                    return match dial().await {
                        Ok((transport, conn)) => {
                            lead.settle(&conn);
                            Ok(Opened::Fresh(transport, conn))
                        }
                        Err(e) => {
                            lead.fail(&e);
                            Err(e)
                        }
                    };
                }
            }
        }
    }
}

/// Upgrade a cached handle, treating a connection that is closed but not yet dropped as absent.
fn open(weak: &iroh::endpoint::WeakConnectionHandle) -> Option<Connection> {
    weak.upgrade().filter(|c| c.close_reason().is_none())
}

/// A leader's claim on the `Dialing` slots of every device it dials. Dropping it unsettled — a
/// failed or cancelled dial — removes those slots; dropping the sender either way wakes waiters.
struct DialLead {
    cache: McpConnCache,
    peers: Vec<[u8; 32]>,
    settled: bool,
    tx: tokio::sync::watch::Sender<Option<Arc<str>>>,
}

impl DialLead {
    /// Cache the winner; the other candidates' slots go (a raced dial's losers were aborted).
    fn settle(mut self, conn: &Connection) {
        let winner = *conn.remote_id().as_bytes();
        {
            let mut map = self.cache.map();
            for peer in &self.peers {
                if *peer == winner {
                    map.insert(*peer, Slot::Live(conn.weak_handle()));
                } else if matches!(map.get(peer), Some(Slot::Dialing(_))) {
                    map.remove(peer);
                }
            }
        }
        self.settled = true;
        // `self` drops here: the sender goes with it and every waiter re-claims, finding `Live`.
    }

    /// Tell waiters why, so one that runs out of time can say more than "timed out".
    fn fail(self, e: &anyhow::Error) {
        self.tx.send_replace(Some(Arc::from(format!("{e:#}"))));
        // `self` drops here and clears the slots.
    }
}

impl Drop for DialLead {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Never `expect` in a destructor: a poisoned lock here would panic during an unwind that
        // is already in progress, and abort the process. The map is plain data, so the value behind
        // the poison is still coherent.
        let mut map = match self.cache.inner.lock() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        for peer in &self.peers {
            if matches!(map.get(peer), Some(Slot::Dialing(_))) {
                map.remove(peer);
            }
        }
    }
}

/// A loopback peer that runs the REAL per-connection mesh path (`run_mesh_connection`) and the
/// ping arm's pong, counting every accepted connection per ALPN. The accepted-connection count is
/// the only honest reading of "how many connections did we open" — `Endpoint::remote_info` reports
/// addresses, not connections, and the cache's own map is the thing under test.
#[cfg(test)]
pub(crate) mod testpeer {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mcpmesh_net::registry::ConnRegistry;
    use mcpmesh_net::{
        ALPN_MCP, ALPN_PING, LiveServices, ServiceEntry, ServiceKind, Services, SessionBackend,
        SessionTransport, TrustGate,
    };

    use crate::allowlist::{AllowlistGate, PeerEntry, PeerStore};

    pub(crate) struct LoopbackPeer {
        pub(crate) id: [u8; 32],
        pub(crate) addr: iroh::EndpointAddr,
        pub(crate) mcp_accepts: Arc<AtomicUsize>,
        pub(crate) ping_accepts: Arc<AtomicUsize>,
        /// The peer's live MCP connections, registered by `run_mesh_connection` and deregistered
        /// when the connection closes — the peer-side reading of "the connection is gone".
        pub(crate) registry: Arc<ConnRegistry>,
        _accept: tokio::task::JoinHandle<()>,
    }

    /// Echoes the (already service-stripped) initialize, then every frame, until the dialer's
    /// half-close.
    struct Echo;

    #[async_trait::async_trait]
    impl SessionBackend for Echo {
        async fn run(
            &self,
            _identity: Option<mcpmesh_net::PeerIdentity>,
            initialize: serde_json::Value,
            mut transport: SessionTransport,
        ) -> anyhow::Result<()> {
            transport.send_value(initialize).await?;
            while let Ok(Some(f)) = transport.recv_value().await {
                transport.send_value(f).await?;
            }
            let _ = transport.shutdown().await;
            Ok(())
        }
    }

    /// Bind a peer on key `[seed; 32]` that trusts `dialer` and serves `services` as
    /// `(name, allow)` pairs, every one backed by [`Echo`].
    pub(crate) async fn loopback_peer(
        dir: &std::path::Path,
        seed: u8,
        dialer: [u8; 32],
        services: &[(&str, &[&str])],
    ) -> LoopbackPeer {
        loopback_peer_with(dir, seed, dialer, services, None, false).await
    }

    /// [`loopback_peer`] that lets the dialer open at most `max_bidi` concurrent streams per
    /// connection — QUIC's stream-credit limit, which this peer advertises and the dialer obeys —
    /// and, with `throttle_ping`, answers every ping with the rate limiter's close instead of a pong.
    pub(crate) async fn loopback_peer_with(
        dir: &std::path::Path,
        seed: u8,
        dialer: [u8; 32],
        services: &[(&str, &[&str])],
        max_bidi: Option<u32>,
        throttle_ping: bool,
    ) -> LoopbackPeer {
        let store = Arc::new(PeerStore::open(&dir.join(format!("peer-{seed}.redb"))).unwrap());
        store
            .add(PeerEntry {
                endpoint_id: dialer,
                nickname: "dialer".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();
        let gate: Arc<dyn TrustGate> = Arc::new(AllowlistGate::new(store));
        let services = Arc::new(LiveServices::new(Arc::new(Services::new(
            services
                .iter()
                .map(|(name, allow)| {
                    (
                        (*name).to_string(),
                        ServiceEntry {
                            backend: Arc::new(Echo),
                            allow: allow.iter().map(|a| (*a).to_string()).collect(),
                            kind: ServiceKind::Run,
                            ephemeral: false,
                        },
                    )
                })
                .collect(),
        ))));
        let hermetic = crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            ..Default::default()
        };
        let endpoint = match max_bidi {
            None => crate::daemon::boot::build_endpoint(
                iroh::SecretKey::from_bytes(&[seed; 32]),
                &hermetic,
                false,
                None,
            )
            .await
            .unwrap(),
            Some(n) => iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .secret_key(iroh::SecretKey::from_bytes(&[seed; 32]))
                .alpns(vec![ALPN_MCP.to_vec(), ALPN_PING.to_vec()])
                .transport_config(
                    iroh::endpoint::QuicTransportConfig::builder()
                        .max_concurrent_bidi_streams(iroh::endpoint::VarInt::from_u32(n))
                        .build(),
                )
                .bind()
                .await
                .unwrap(),
        };
        let id = *endpoint.id().as_bytes();
        let addr = endpoint.addr();
        let mcp_accepts = Arc::new(AtomicUsize::new(0));
        let ping_accepts = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(ConnRegistry::new());
        let accept = {
            let (endpoint, mcp, ping, registry) = (
                endpoint.clone(),
                mcp_accepts.clone(),
                ping_accepts.clone(),
                registry.clone(),
            );
            tokio::spawn(async move {
                while let Some(incoming) = endpoint.accept().await {
                    let Ok(conn) = incoming.await else { continue };
                    let alpn = conn.alpn().to_vec();
                    if alpn == ALPN_MCP {
                        mcp.fetch_add(1, Ordering::SeqCst);
                        let (gate, services, registry) =
                            (gate.clone(), services.clone(), registry.clone());
                        tokio::spawn(mcpmesh_net::run_mesh_connection(
                            conn, gate, services, registry,
                        ));
                    } else if alpn == ALPN_PING && throttle_ping {
                        ping.fetch_add(1, Ordering::SeqCst);
                        conn.close(0u32.into(), crate::daemon::reach::PING_THROTTLE_CLOSE);
                    } else if alpn == ALPN_PING {
                        ping.fetch_add(1, Ordering::SeqCst);
                        tokio::spawn(async move {
                            if let Ok((mut send, _recv)) = conn.accept_bi().await {
                                let pong = serde_json::json!({
                                    "stack_version": "test",
                                    "services": ["echo"],
                                });
                                if mcpmesh_net::framing::write_frame(&mut send, &pong)
                                    .await
                                    .is_ok()
                                {
                                    let _ = send.finish();
                                    let _ = send.stopped().await;
                                }
                            }
                        });
                    } else {
                        conn.close(0u32.into(), b"unexpected alpn");
                    }
                }
            })
        };
        LoopbackPeer {
            id,
            addr,
            mcp_accepts,
            ping_accepts,
            registry,
            _accept: accept,
        }
    }

    /// A hermetic dialer mesh with `peer` stored as nickname `bob`, hinted at its real address. Its
    /// endpoint carries the ARMED #229 hooks, as a booted node's does, so the revoke close pass and
    /// the dial veto are the real ones.
    pub(crate) async fn dialer_mesh(
        dir: &std::path::Path,
        peer: &LoopbackPeer,
    ) -> Arc<crate::daemon::MeshState> {
        let cfg = dir.join("dialer.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_hooked_mesh(cfg).await;
        mesh.store
            .add(PeerEntry {
                endpoint_id: peer.id,
                nickname: "bob".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: Some(serde_json::to_string(&peer.addr).unwrap()),
            })
            .unwrap();
        mesh
    }

    /// The dialer's own endpoint id — `hermetic_mesh` binds on `[7u8; 32]`.
    pub(crate) fn dialer_id() -> [u8; 32] {
        *iroh::SecretKey::from_bytes(&[7u8; 32]).public().as_bytes()
    }

    /// The dialer's `eid:` principal, for a peer's allow list.
    pub(crate) fn dialer_principal() -> String {
        mcpmesh_net::EndpointId::from_bytes(dialer_id()).principal()
    }

    /// Send a service-named `initialize` and return the first frame back.
    pub(crate) async fn first_reply(t: &mut SessionTransport, service: &str) -> serde_json::Value {
        let init = crate::daemon::dial::inject_service(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            service,
        );
        t.send_value(init).await.expect("send initialize");
        tokio::time::timeout(std::time::Duration::from_secs(10), t.recv_value())
            .await
            .expect("a reply within 10s")
            .expect("a readable reply")
            .expect("a frame, not EOF")
    }

    /// Wait, sleeping, up to 10s for `cond` — never a `yield_now` spin.
    pub(crate) async fn eventually(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        cond()
    }
}

#[cfg(test)]
mod tests {
    /// The `refused` predicate for tests that drive `session_on` directly over devices nothing
    /// revokes.
    async fn never_refused(_: [u8; 32]) -> anyhow::Result<bool> {
        Ok(false)
    }
    use std::sync::atomic::Ordering;

    use super::testpeer::{
        dialer_id, dialer_mesh, dialer_principal, eventually, first_reply, loopback_peer,
    };
    use crate::daemon::dial::dial_service;

    /// #215, the property: two sessions to one peer are two bi-streams on ONE connection.
    ///
    /// Asserted on the PEER's accepted-connection count — the one observable the cache cannot
    /// fake — and on both sessions round-tripping independently, so a "reuse" that shared a
    /// stream would also fail. Mutation: bypass the cache in `dial_single` (always dial) → two
    /// accepts.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_sessions_to_one_peer_share_one_connection() {
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 41, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;

        let mut a = dial_service(&mesh, "bob", "echo").await.expect("session A");
        let mut b = dial_service(&mesh, "bob", "echo").await.expect("session B");
        // Round-trip BEFORE counting: the peer counts an accept on its own task, so a count read
        // the instant the dial returns can lag. A reply on B proves the peer has already served
        // B's stream — and therefore already counted B's connection, if B had its own.
        let ra = first_reply(&mut a, "echo").await;
        let rb = first_reply(&mut b, "echo").await;
        assert_eq!(
            ra["method"], "initialize",
            "session A echoes on its own stream: {ra}"
        );
        assert_eq!(
            rb["method"], "initialize",
            "session B echoes on its own stream: {rb}"
        );
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            1,
            "a second session to the same peer must ride the first session's connection"
        );
    }

    /// Weak semantics: no session, no connection. The cache must not keep a warm idle connection —
    /// that would be a NEW connection lifetime nobody configured, and iroh#4390 counts live client
    /// connections. Read on the PEER: its registry drops the connection when `accept_bi` ends.
    /// Mutation: hold a strong `Connection` in `Slot::Live` → the peer's registry stays at 1 and
    /// the bounded wait expires.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_shared_connection_closes_when_its_last_session_ends() {
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 42, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;

        let a = dial_service(&mesh, "bob", "echo").await.expect("session A");
        let b = dial_service(&mesh, "bob", "echo").await.expect("session B");
        assert!(
            eventually(|| peer.registry.len() == 1).await,
            "precondition: the peer tracks the one shared connection"
        );
        drop(a);
        assert!(
            mesh.conn_cache.live(peer.id).is_some(),
            "one session still holds the connection open"
        );
        drop(b);
        assert!(
            eventually(|| peer.registry.is_empty()).await,
            "the peer must see the connection close once the last session ends — a strong handle \
             in the cache keeps it open forever"
        );
        assert!(
            mesh.conn_cache.live(peer.id).is_none(),
            "and the cache must report no live connection"
        );
    }

    /// A closed connection whose handle still upgrades (session A is still holding its streams)
    /// must not be reused. The peer closes it — a sever, the realistic case — and the next session
    /// must dial fresh and WORK. Values: a reused stale handle fails `open_bi` and the dial errors;
    /// a fresh dial succeeds and the peer's accept count reads 2. Mutation: in `open`, drop the
    /// `close_reason().is_none()` filter AND make `open_on` return the error instead of falling
    /// through → session B fails.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_closed_connection_is_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 43, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;

        let mut a = dial_service(&mesh, "bob", "echo").await.expect("session A");
        assert!(eventually(|| peer.registry.len() == 1).await);
        // The peer severs it (what a revoke does), and we wait until OUR side has observed the
        // close — the stream read fails — so the stale handle is genuinely closed, not merely
        // about to be.
        assert_eq!(
            peer.registry
                .sever_matching(401, b"test sever", |_, _| true),
            1
        );
        let observed = tokio::time::timeout(std::time::Duration::from_secs(10), a.recv_value())
            .await
            .expect("the close reaches the dialer within 10s");
        assert!(
            !matches!(observed, Ok(Some(_))),
            "precondition: session A's stream is dead: {observed:?}"
        );

        let mut b = dial_service(&mesh, "bob", "echo")
            .await
            .expect("a fresh dial after the peer closed the pooled connection");
        assert_eq!(first_reply(&mut b, "echo").await["method"], "initialize");
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            2,
            "session B must be a NEW connection, not a stream on the closed one"
        );
        drop(a);
    }

    /// #166 sessions are never pooled, in either direction: the timed session gets its own
    /// connection (accept 1), a plain session after it does not join it (accept 2), a second plain
    /// session joins the plain one (still 2), and a second timed session dials again (3).
    /// Mutation: drop the `per_conn.is_some()` early return in `dial_single` → the plain session
    /// rides the timed connection and the count stays 1.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_with_its_own_idle_timeout_keeps_its_own_connection() {
        use crate::daemon::dial::dial_service_with_idle_timeout;
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 44, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let secs = mesh.keep_alive_secs() + 25;

        let mut timed = dial_service_with_idle_timeout(&mesh, "bob", "echo", Some(secs))
            .await
            .expect("timed session");
        assert_eq!(
            first_reply(&mut timed, "echo").await["method"],
            "initialize"
        );
        assert_eq!(peer.mcp_accepts.load(Ordering::SeqCst), 1);
        assert!(
            mesh.conn_cache.live(peer.id).is_none(),
            "a per-session connection must never enter the shared cache"
        );
        let mut plain = dial_service(&mesh, "bob", "echo")
            .await
            .expect("plain session");
        assert_eq!(
            first_reply(&mut plain, "echo").await["method"],
            "initialize"
        );
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            2,
            "a plain session must not join a connection carrying someone else's idle timeout"
        );
        let mut plain2 = dial_service(&mesh, "bob", "echo")
            .await
            .expect("second plain session");
        assert_eq!(
            first_reply(&mut plain2, "echo").await["method"],
            "initialize"
        );
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            2,
            "…but it does share the plain connection"
        );
        let mut timed2 = dial_service_with_idle_timeout(&mesh, "bob", "echo", Some(secs))
            .await
            .expect("second timed session");
        assert_eq!(
            first_reply(&mut timed2, "echo").await["method"],
            "initialize"
        );
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            3,
            "and a second timed session dials its own connection rather than reusing the plain one"
        );
        drop((timed, plain, plain2, timed2));
    }

    /// The first-dial race: two callers, no live connection, both claim at once. The single-flight
    /// slot makes the second WAIT rather than dial. Deterministic: the leader's dial is held on a
    /// gate the test controls, so the follower provably arrives while the dial is in flight; its
    /// own dial closure counts how often it is invoked. Mutation: make `claim` return `Lead` for a
    /// `Dialing` slot (no waiting) → the follower dials, two accepts.
    #[tokio::test(flavor = "multi_thread")]
    async fn simultaneous_first_dials_share_one_connection() {
        use crate::daemon::dial::{DIAL_TIMEOUT, connect_with_timeout};
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 45, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let follower_dials = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let leader = {
            let (mesh, addr) = (mesh.clone(), peer.addr.clone());
            let endpoint = mesh.endpoint.clone();
            tokio::spawn(async move {
                mesh.conn_cache
                    .session_on(&[peer.id], DIAL_TIMEOUT, never_refused, || async move {
                        let _ = gate.await;
                        connect_with_timeout(&endpoint, addr, "echo", DIAL_TIMEOUT, None).await
                    })
                    .await
            })
        };
        // Let the leader claim its slot (its dial is parked on the gate).
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let follower = {
            let (mesh, addr, dials) = (mesh.clone(), peer.addr.clone(), follower_dials.clone());
            let endpoint = mesh.endpoint.clone();
            tokio::spawn(async move {
                mesh.conn_cache
                    .session_on(&[peer.id], DIAL_TIMEOUT, never_refused, || async move {
                        dials.fetch_add(1, Ordering::SeqCst);
                        connect_with_timeout(&endpoint, addr, "echo", DIAL_TIMEOUT, None).await
                    })
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            0,
            "nothing is dialled while the leader's dial is parked — the follower must be waiting"
        );
        release.send(()).unwrap();
        let (l, f) = tokio::join!(leader, follower);
        let l = l.unwrap().expect("leader session");
        let f = f.unwrap().expect("follower session");
        assert!(matches!(l, super::Opened::Fresh(..)), "the leader dialled");
        let mut f = match f {
            super::Opened::Reused(t) => t,
            super::Opened::Fresh(..) => panic!("the follower must reuse, not dial"),
        };
        assert_eq!(first_reply(&mut f, "echo").await["method"], "initialize");
        assert_eq!(
            follower_dials.load(Ordering::SeqCst),
            0,
            "the follower never dialled"
        );
        assert_eq!(peer.mcp_accepts.load(Ordering::SeqCst), 1);
    }

    /// A leader that FAILS must not strand its waiters: the slot is cleared on drop and the waiter
    /// dials for itself. Mutation: remove the slot removal in `DialLead::drop` → the follower
    /// waits forever (bounded here by the timeout).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_leader_releases_its_waiters() {
        use crate::daemon::dial::{DIAL_TIMEOUT, connect_with_timeout};
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 46, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let (release, gate) = tokio::sync::oneshot::channel::<()>();

        let leader = {
            let mesh = mesh.clone();
            tokio::spawn(async move {
                mesh.conn_cache
                    .session_on(&[peer.id], DIAL_TIMEOUT, never_refused, || async move {
                        let _ = gate.await;
                        anyhow::bail!("leader's dial failed")
                    })
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let follower = {
            let (mesh, addr) = (mesh.clone(), peer.addr.clone());
            let endpoint = mesh.endpoint.clone();
            tokio::spawn(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    mesh.conn_cache.session_on(
                        &[peer.id],
                        DIAL_TIMEOUT,
                        never_refused,
                        || async move {
                            connect_with_timeout(&endpoint, addr, "echo", DIAL_TIMEOUT, None).await
                        },
                    ),
                )
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        release.send(()).unwrap();
        assert!(leader.await.unwrap().is_err());
        let f = follower
            .await
            .unwrap()
            .expect("the follower must be woken when the leader fails")
            .expect("and then dial for itself");
        let mut f = match f {
            super::Opened::Fresh(t, _) => t,
            super::Opened::Reused(_) => panic!("nothing was live to reuse"),
        };
        assert_eq!(first_reply(&mut f, "echo").await["method"], "initialize");
        assert_eq!(peer.mcp_accepts.load(Ordering::SeqCst), 1);
    }

    /// A leader CANCELLED mid-dial — its `open_session` control connection dropped, its future
    /// with it — must not strand its waiters either. Same `Drop` path as a failed leader, pinned
    /// separately because the module doc claims both and a cancelled future never reaches the
    /// `Err` arm. Mutation: remove the slot removal in `DialLead::drop` → the follower waits out
    /// the 10s bound.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_leader_releases_its_waiters() {
        use crate::daemon::dial::{DIAL_TIMEOUT, connect_with_timeout};
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 51, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;

        let leader = {
            let mesh = mesh.clone();
            tokio::spawn(async move {
                mesh.conn_cache
                    .session_on(&[peer.id], DIAL_TIMEOUT, never_refused, || async move {
                        std::future::pending::<()>().await; // parked until aborted
                        unreachable!("a parked dial never completes")
                    })
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let follower = {
            let (mesh, addr) = (mesh.clone(), peer.addr.clone());
            let endpoint = mesh.endpoint.clone();
            tokio::spawn(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    mesh.conn_cache.session_on(
                        &[peer.id],
                        DIAL_TIMEOUT,
                        never_refused,
                        || async move {
                            connect_with_timeout(&endpoint, addr, "echo", DIAL_TIMEOUT, None).await
                        },
                    ),
                )
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        leader.abort();
        match leader.await {
            Err(e) => assert!(e.is_cancelled()),
            Ok(_) => panic!("the parked leader cannot have completed"),
        }
        let f = follower
            .await
            .unwrap()
            .expect("the follower must be woken when the leader is cancelled")
            .expect("and then dial for itself");
        let mut f = match f {
            super::Opened::Fresh(t, _) => t,
            super::Opened::Reused(_) => panic!("nothing was live to reuse"),
        };
        assert_eq!(first_reply(&mut f, "echo").await["method"], "initialize");
        assert_eq!(peer.mcp_accepts.load(Ordering::SeqCst), 1);
    }

    /// Nothing about authz changes: the accept path resolves the allow list PER BI-STREAM, so a
    /// second session on the shared connection to a service the caller is not granted is refused
    /// with -32054 exactly as a fresh connection would be — and it IS the shared connection
    /// (accept count 1). Mutation (fixture): add the dialer's principal to `private`'s allow →
    /// the refusal assertion fails, proving it discriminates. Mutation (code): bypass the cache →
    /// accept count 2.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_stream_on_a_shared_connection_is_still_authorized_per_stream() {
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(
            dir.path(),
            47,
            dialer_id(),
            &[("echo", &[me.as_str()]), ("private", &["eid:nobody"])],
        )
        .await;
        let mesh = dialer_mesh(dir.path(), &peer).await;

        let mut ok = dial_service(&mesh, "bob", "echo")
            .await
            .expect("granted session");
        let reply = first_reply(&mut ok, "echo").await;
        assert_eq!(
            reply["method"], "initialize",
            "the granted service answers: {reply}"
        );

        let mut refused = dial_service(&mesh, "bob", "private")
            .await
            .expect("stream opens");
        let reply = first_reply(&mut refused, "private").await;
        assert_eq!(
            reply["error"]["code"],
            mcpmesh_net::errors::ERR_SERVICE,
            "an ungranted service on a SHARED connection must still be refused: {reply}"
        );
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            1,
            "and it was the shared connection that refused it"
        );
    }

    /// The racing paths (a `b64u:` user with several devices): a live connection to any candidate
    /// device is reused without racing, and a race's WINNER is recorded so the next session reuses
    /// it. Mutation: `DialLead::settle` not caching the winner → the second dial races again
    /// (accept 2). Mutation: `claim` skipping `Live` slots → same.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_raced_dial_reuses_and_records_the_winner() {
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 48, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        // A second device of the same person that nobody answers for. The store must hold ONLY
        // the two `b64u:bob` rows, so the `b64u:` path races rather than resolving one entry.
        let dead = *iroh::SecretKey::from_bytes(&[49u8; 32]).public().as_bytes();
        assert!(mesh.store.remove("bob").unwrap());
        for (eid, nick, hint) in [
            (dead, "bob-dead", None),
            (
                peer.id,
                "bob-live",
                Some(serde_json::to_string(&peer.addr).unwrap()),
            ),
        ] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: eid,
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: Some("b64u:bob".into()),
                    last_addr: hint,
                })
                .unwrap();
        }
        let mut a = dial_service(&mesh, "b64u:bob", "echo")
            .await
            .expect("raced session");
        assert_eq!(first_reply(&mut a, "echo").await["method"], "initialize");
        assert_eq!(peer.mcp_accepts.load(Ordering::SeqCst), 1);
        let mut b = dial_service(&mesh, "b64u:bob", "echo")
            .await
            .expect("second session");
        assert_eq!(first_reply(&mut b, "echo").await["method"], "initialize");
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            1,
            "the race's winner must be recorded and reused — racing again is a second connection"
        );
        drop((a, b));
    }

    /// Store a `b64u:bob` person with two live devices, X and Y, and nothing else — so a
    /// `b64u:bob` dial races them rather than resolving a single entry.
    async fn two_device_person(
        dir: &std::path::Path,
        seeds: (u8, u8),
    ) -> (
        super::testpeer::LoopbackPeer,
        super::testpeer::LoopbackPeer,
        std::sync::Arc<crate::daemon::MeshState>,
    ) {
        let me = dialer_principal();
        let x = loopback_peer(dir, seeds.0, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let y = loopback_peer(dir, seeds.1, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir, &x).await;
        assert!(mesh.store.remove("bob").unwrap());
        for (p, nick) in [(&x, "bob-x"), (&y, "bob-y")] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: p.id,
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: Some("b64u:bob".into()),
                    last_addr: Some(serde_json::to_string(&p.addr).unwrap()),
                })
                .unwrap();
        }
        (x, y, mesh)
    }

    fn eid(peer: &super::testpeer::LoopbackPeer) -> String {
        format!("eid:{}", data_encoding::HEXLOWER.encode(&peer.id))
    }

    /// A raced dial never reuses a connection to a revoked device (#215 review M3). The revocation
    /// is a bare store write, so #229's close pass never runs and X's connection stays warm; two
    /// guards remain — `hinted_addrs` drops X before the cache is consulted, and the cache refuses X
    /// at hand-out. A reused X would answer (X accepts 1, Y accepts 0); the correct dial reaches Y
    /// fresh. Mutation: consult the cache over the raw candidates AND drop the hand-out check → X is
    /// reused. Either mutation alone passes: each guard covers the other here.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_raced_dial_never_reuses_a_connection_to_a_revoked_device() {
        let dir = tempfile::tempdir().unwrap();
        let (x, y, mesh) = two_device_person(dir.path(), (60, 61)).await;

        let mut a = dial_service(&mesh, &eid(&x), "echo")
            .await
            .expect("session to X");
        assert_eq!(first_reply(&mut a, "echo").await["method"], "initialize");
        assert!(
            mesh.conn_cache.live(x.id).is_some(),
            "precondition: X is cached"
        );
        mesh.store
            .revoke(crate::allowlist::RevokedEntry {
                endpoint_id: x.id,
                revoked_at: 0,
                reason: None,
                source: "local".into(),
                signer_user_id: None,
                issued_at: None,
            })
            .unwrap();
        assert!(
            mesh.conn_cache.live(x.id).is_some(),
            "precondition: a bare store write leaves X's connection warm"
        );

        let mut b = dial_service(&mesh, "b64u:bob", "echo")
            .await
            .expect("the person is still reachable on Y");
        assert_eq!(first_reply(&mut b, "echo").await["method"], "initialize");
        assert_eq!(
            y.mcp_accepts.load(Ordering::SeqCst),
            1,
            "the session must go to Y — reusing X hands the request to a revoked device"
        );
        assert_eq!(x.mcp_accepts.load(Ordering::SeqCst), 1, "X got nothing new");
        drop(a);
    }

    /// The cache asks `dial_refused` at the moment it hands out a connection, not only the dial path
    /// before it (#215 with #229). The window: a session passes `refuse_if_revoked`, then WAITS on
    /// another caller's dial to the same device; the device is revoked meanwhile, through the store
    /// alone, so no revoke verb runs and #229's close pass never sees it; the leader's connection —
    /// handshaken before the revoke, so neither hook refused it — lands in the cache. The waiter must
    /// be refused, not handed that connection. Mutation: drop the `refused(peer)` arm in
    /// `claim_loop` → the waiter gets a stream on the revoked device.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_that_waited_is_not_handed_a_connection_to_a_device_revoked_meanwhile() {
        use crate::daemon::dial::{DIAL_TIMEOUT, connect_with_timeout};
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 70, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let (release, gate) = tokio::sync::oneshot::channel::<()>();

        // The leader CONNECTS first and only then parks, so its handshake — and both #229 hook
        // checks — complete before the revocation exists.
        let leader = {
            let (mesh, addr) = (mesh.clone(), peer.addr.clone());
            let endpoint = mesh.endpoint.clone();
            tokio::spawn(async move {
                mesh.conn_cache
                    .session_on(&[peer.id], DIAL_TIMEOUT, never_refused, || async move {
                        let opened =
                            connect_with_timeout(&endpoint, addr, "echo", DIAL_TIMEOUT, None).await;
                        let _ = gate.await;
                        opened
                    })
                    .await
            })
        };
        assert!(
            eventually(|| peer.mcp_accepts.load(Ordering::SeqCst) == 1).await,
            "precondition: the leader's connection is up before the revoke"
        );
        let follower = {
            let mesh = mesh.clone();
            let target = eid(&peer);
            tokio::spawn(async move { dial_service(&mesh, &target, "echo").await })
        };
        // Let the follower pass `refuse_if_revoked` and park on the leader's Dialing slot.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        mesh.store
            .revoke(crate::allowlist::RevokedEntry {
                endpoint_id: peer.id,
                revoked_at: 0,
                reason: None,
                source: "local".into(),
                signer_user_id: None,
                issued_at: None,
            })
            .unwrap();
        release.send(()).unwrap();

        let _leader = leader
            .await
            .unwrap()
            .expect("the leader's own dial predates the revoke");
        let got = tokio::time::timeout(std::time::Duration::from_secs(10), follower)
            .await
            .expect("the follower resolves within 10s")
            .unwrap();
        let err = match got {
            Ok(_) => panic!(
                "a session that waited must not be handed a connection to a device revoked while \
                 it waited"
            ),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("REVOKED"), "and it says why: {err}");
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            1,
            "nothing new reached the revoked device"
        );
    }

    /// A revocation check that FAILS (a join error on the blocking pool) refuses the reuse and says
    /// the check failed — never that a device nobody revoked is REVOKED. Mutation: fold the error
    /// into `Ok(true)` → the message claims revocation.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_revocation_check_refuses_reuse_without_calling_the_device_revoked() {
        use crate::daemon::dial::{DIAL_TIMEOUT, connect_with_timeout};
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 71, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let (endpoint, addr) = (mesh.endpoint.clone(), peer.addr.clone());
        let _first = mesh
            .conn_cache
            .session_on(&[peer.id], DIAL_TIMEOUT, never_refused, || async move {
                connect_with_timeout(&endpoint, addr, "echo", DIAL_TIMEOUT, None).await
            })
            .await
            .expect("first session caches a connection");

        let err = match mesh
            .conn_cache
            .session_on(
                &[peer.id],
                DIAL_TIMEOUT,
                |_| async { Err(anyhow::anyhow!("join error")) },
                || async { unreachable!("a live connection is claimed, nothing dials") },
            )
            .await
        {
            Ok(_) => panic!("a failed check must refuse the reuse"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("revocation check failed"), "{err}");
        assert!(!err.contains("REVOKED"), "must not claim revocation: {err}");
    }

    /// `peer_revoke` closes the session THIS node opened to the device, not only the ones it
    /// accepted (#215 review M3). Read on the device's side: its registry empties when our
    /// connection closes. Mutation: delete #229's `close_refused_peer_conns` call in `sever_principals` → the connection
    /// stays up and the 10s wait expires.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_revoke_closes_this_nodes_own_connection_to_the_device() {
        let dir = tempfile::tempdir().unwrap();
        let (x, _y, mesh) = two_device_person(dir.path(), (62, 63)).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let mut a = dial_service(&mesh, &eid(&x), "echo")
            .await
            .expect("session to X");
        assert_eq!(first_reply(&mut a, "echo").await["method"], "initialize");
        assert!(eventually(|| x.registry.len() == 1).await);

        crate::daemon::handlers::peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "bob-x".into(),
                reason: None,
            },
        )
        .await
        .expect("revoke");

        assert!(
            eventually(|| x.registry.is_empty()).await,
            "the revoked device must see our session connection close — a warm connection to it \
             is exactly what the next dial would otherwise be handed"
        );
        assert!(
            mesh.conn_cache.live(x.id).is_none(),
            "and the cache forgot it"
        );
        let observed = tokio::time::timeout(std::time::Duration::from_secs(10), a.recv_value())
            .await
            .expect("the close reaches session A within 10s");
        assert!(
            !matches!(observed, Ok(Some(_))),
            "session A is ended by the revoke: {observed:?}"
        );
    }

    /// #215 review S4: a shared connection that has run out of stream credit must not stall the
    /// next session. The peer allows ONE concurrent stream per connection, so session B's `open_bi`
    /// on A's connection waits for credit that only A's end would return. Before the fix that wait
    /// was the whole `DIAL_TIMEOUT` (20s) and then a misleading "dial timed out" on the single-target
    /// path, and unbounded on the racing path. Now a short bounded wait falls back to a fresh,
    /// uncached connection. Mutation: remove the open timeout → both 10s bounds expire. Mutation:
    /// treat `Saturated` as `Stale` → B's connection replaces A's in the cache.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_saturated_shared_connection_falls_back_to_a_fresh_connection() {
        use super::testpeer::loopback_peer_with;
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer_with(
            dir.path(),
            64,
            dialer_id(),
            &[("echo", &[me.as_str()])],
            Some(1),
            false,
        )
        .await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let bound = std::time::Duration::from_secs(10);

        let mut a = dial_service(&mesh, "bob", "echo").await.expect("session A");
        assert_eq!(first_reply(&mut a, "echo").await["method"], "initialize");
        let a_conn = mesh
            .conn_cache
            .live(peer.id)
            .expect("A's connection is cached")
            .stable_id();

        let mut b = tokio::time::timeout(bound, dial_service(&mesh, "bob", "echo"))
            .await
            .expect("session B must not wait out the dial timeout on a saturated connection")
            .expect("session B opens");
        assert_eq!(first_reply(&mut b, "echo").await["method"], "initialize");
        assert_eq!(
            peer.mcp_accepts.load(Ordering::SeqCst),
            2,
            "B rides its own connection, because A's has no stream credit left"
        );
        assert_eq!(
            mesh.conn_cache.live(peer.id).map(|c| c.stable_id()),
            Some(a_conn),
            "and A's connection is still the cached one — saturation is not staleness, and B's \
             overflow connection must not replace it"
        );
    }

    /// S4 on the RACING path, where the reuse loop had no bound at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_saturated_candidate_does_not_hang_a_raced_dial() {
        use super::testpeer::loopback_peer_with;
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let x = loopback_peer_with(
            dir.path(),
            65,
            dialer_id(),
            &[("echo", &[me.as_str()])],
            Some(1),
            false,
        )
        .await;
        let mesh = dialer_mesh(dir.path(), &x).await;
        assert!(mesh.store.remove("bob").unwrap());
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: x.id,
                nickname: "bob-x".into(),
                services: vec![],
                paired_at: None,
                user_id: Some("b64u:bob".into()),
                last_addr: Some(serde_json::to_string(&x.addr).unwrap()),
            })
            .unwrap();
        // A second device nobody answers for, so `b64u:bob` takes the racing path.
        let dead = *iroh::SecretKey::from_bytes(&[66u8; 32]).public().as_bytes();
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: dead,
                nickname: "bob-dead".into(),
                services: vec![],
                paired_at: None,
                user_id: Some("b64u:bob".into()),
                last_addr: None,
            })
            .unwrap();

        let mut a = dial_service(&mesh, &eid(&x), "echo")
            .await
            .expect("session A");
        assert_eq!(first_reply(&mut a, "echo").await["method"], "initialize");
        let mut b = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            dial_service(&mesh, "b64u:bob", "echo"),
        )
        .await
        .expect("a raced dial must not hang on a saturated candidate connection")
        .expect("session B opens");
        assert_eq!(first_reply(&mut b, "echo").await["method"], "initialize");
    }

    /// #215 review S5: the RACING path single-flights too. A dial to one of the person's devices is
    /// in flight (held on a gate the test controls); a `b64u:bob` session arriving meanwhile must
    /// WAIT for it rather than race its own — the loser of that race would live as long as its
    /// session, a sustained second connection. Deterministic: nothing may be accepted while the gate
    /// is closed. Mutation: `race_or_reuse` not consulting the cache's claims → it dials at once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_raced_dial_waits_for_a_dial_already_in_flight() {
        use crate::daemon::dial::{DIAL_TIMEOUT, connect_with_timeout};
        let dir = tempfile::tempdir().unwrap();
        let (x, y, mesh) = two_device_person(dir.path(), (67, 68)).await;
        let (release, gate) = tokio::sync::oneshot::channel::<()>();

        let leader = {
            let (mesh, addr) = (mesh.clone(), x.addr.clone());
            let endpoint = mesh.endpoint.clone();
            tokio::spawn(async move {
                mesh.conn_cache
                    .session_on(&[x.id], DIAL_TIMEOUT, never_refused, || async move {
                        let _ = gate.await;
                        connect_with_timeout(&endpoint, addr, "echo", DIAL_TIMEOUT, None).await
                    })
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let follower = {
            let mesh = mesh.clone();
            tokio::spawn(async move { dial_service(&mesh, "b64u:bob", "echo").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert_eq!(
            x.mcp_accepts.load(Ordering::SeqCst) + y.mcp_accepts.load(Ordering::SeqCst),
            0,
            "a raced dial must wait on the device dial already in flight, not race beside it"
        );
        release.send(()).unwrap();
        let _l = leader.await.unwrap().expect("leader");
        let mut f = follower.await.unwrap().expect("follower session");
        assert_eq!(first_reply(&mut f, "echo").await["method"], "initialize");
        assert_eq!(
            x.mcp_accepts.load(Ordering::SeqCst) + y.mcp_accepts.load(Ordering::SeqCst),
            1,
            "one connection for both sessions"
        );
    }

    /// `open_on` forgets an entry only if it still names the connection that failed. Between a
    /// caller's claim and its failed `open_bi`, another caller can cache a NEWER connection to the
    /// same peer; removing that one would throw away a healthy connection and make the next session
    /// dial a second. Mutation: drop the `stable_id` comparison → the newer entry is removed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_open_never_forgets_a_newer_connection() {
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 69, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let old = mesh
            .endpoint
            .connect(peer.addr.clone(), mcpmesh_net::ALPN_MCP)
            .await
            .expect("old connection");
        let newer = mesh
            .endpoint
            .connect(peer.addr.clone(), mcpmesh_net::ALPN_MCP)
            .await
            .expect("newer connection");
        let cache = super::McpConnCache::new();
        cache
            .map()
            .insert(peer.id, super::Slot::Live(newer.weak_handle()));
        old.close(0u32.into(), b"gone");

        assert!(matches!(
            cache.open_on(peer.id, &old).await,
            super::Stream::Stale
        ));
        assert_eq!(
            cache.live(peer.id).map(|c| c.stable_id()),
            Some(newer.stable_id()),
            "the failure of an OLD connection must not evict the newer one cached meanwhile"
        );
    }

    /// A waiter that runs out of time reports the failure of the dial it waited on, not a bare
    /// "timed out" — the leader had already learned the peer refused. Values: the leader fails with
    /// a distinctive reason; the waiter's own dial never completes, so its deadline is what ends it.
    /// Mutation: stop recording the leader's failure → the message lacks the reason.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_waiter_that_times_out_reports_the_leaders_failure() {
        let peer = [0x7Au8; 32];
        let cache = super::McpConnCache::new();
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let leader = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .session_on(
                        &[peer],
                        std::time::Duration::from_secs(10),
                        never_refused,
                        || async move {
                            let _ = gate.await;
                            anyhow::bail!("the peer refused: leader-reason-7a")
                        },
                    )
                    .await
                    .map(|_| ())
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let follower = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .session_on(
                        &[peer],
                        std::time::Duration::from_secs(2),
                        never_refused,
                        || async move {
                            std::future::pending::<()>().await;
                            unreachable!("a parked dial never completes")
                        },
                    )
                    .await
                    .map(|_| ())
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        release.send(()).unwrap();
        assert!(leader.await.unwrap().is_err());
        let err = follower
            .await
            .unwrap()
            .expect_err("the follower's own dial never completes");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("dial timed out") && msg.contains("leader-reason-7a"),
            "the timeout must carry the failure it waited on: {msg}"
        );
    }

    /// A dial lead dropped while the cache lock is POISONED must not panic: its `Drop` runs during
    /// unwinds, and a panic there aborts the process. Mutation: `expect` the lock in `Drop` → the
    /// drop panics.
    #[test]
    fn dropping_a_dial_lead_survives_a_poisoned_lock() {
        let cache = super::McpConnCache::new();
        let peer = [0x7Bu8; 32];
        let super::Claim::Lead(lead) = cache.claim(&[peer]) else {
            panic!("an empty cache must hand out a lead");
        };
        let poisoner = cache.clone();
        let _ = std::thread::spawn(move || {
            let _held = poisoner.inner.lock().unwrap();
            panic!("poison the conn cache lock");
        })
        .join();
        assert!(
            cache.inner.is_poisoned(),
            "precondition: the lock is poisoned"
        );
        let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(lead)));
        assert!(
            dropped.is_ok(),
            "a lead's Drop must not panic on a poisoned lock"
        );
        let slots = match cache.inner.lock() {
            Ok(m) => m.len(),
            Err(p) => p.into_inner().len(),
        };
        assert_eq!(slots, 0, "and it still clears its Dialing slot");
    }
}
