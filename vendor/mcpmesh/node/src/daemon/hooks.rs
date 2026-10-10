//! The endpoint hooks (#229): the OUTBOUND revocation veto and the per-peer connection registry
//! that lets a revoke close connections this node OPENED.
//!
//! **Why hooks.** Every mcpmesh dial path filters through [`dial_refused`] before it calls
//! `Endpoint::connect`, but iroh-gossip dials peers it LEARNS from the swarm (ForwardJoin, Shuffle)
//! itself (`iroh-gossip` 0.101 `net.rs` `Dialer::queue_dial`), through the same `Endpoint::connect`.
//! iroh 1.2.0 runs [`EndpointHooks::before_connect`] inside `connect_with_opts` before any packet is
//! sent (`endpoint.rs:1115`), so one hook covers gossip, the roster blob, MCP, ping, app blobs and
//! every embedder protocol, including dials no mcpmesh code makes.
//!
//! **What is gated.** Every ALPN except [`ALPN_PAIR`]. Pairing is authenticated by the invite
//! secret, dials strangers by design, and its dial paths run their own
//! [`PeerStore::is_refused`] check. Everything else is a connection to a peer's device, whatever
//! protocol it speaks, so it is refused when [`dial_refused`] refuses the device.
//!
//! **Fail closed until armed.** The gate's inputs (store + roster gate) exist only after the endpoint
//! is bound, so the hook starts UNARMED and refuses every gated dial. `boot_node` arms it right after
//! the store and roster gate are built and before gossip subscribes with its bootstrap set, which
//! is the first gated dial a boot makes.
//!
//! **Never the `Endpoint`.** Hooks live on the endpoint, so a hook holding one is a reference cycle
//! that leaks it (`hooks.rs:60-64`). This holds a store, a roster gate and the registry, none of
//! which reaches the endpoint. Connections are held as [`WeakConnectionHandle`]s, so the registry
//! never keeps a connection alive or disables close-on-drop.
//!
//! **Blocking.** The hooks are async; [`dial_refused`]'s redb reads run on the blocking pool. Gossip
//! dials run in the Dialer's own `JoinSet`, not on the gossip actor loop, so an awaiting hook never
//! stalls the actor (iroh-gossip#155).
//!
//! **One registry for every OUTBOUND sever.** Connections this node DIALS — `open_session`,
//! embedder `connect_protocol` connections, gossip links it opened — land here; [`close_refused`] is
//! what the revoke paths call. INBOUND connections are not registered: the accept loop's
//! `ConnRegistry` already tracks and severs them, and counts them in `severed`. It is also what closes
//! the #215 MCP connection cache's shared connections on revoke: the cache has no close path of its
//! own, and a connection this pass closes reads as dead to it.
//!
//! **Cost.** Nothing runs for an inbound connection, so a stranger's handshake costs this module
//! nothing. Each outbound non-pair dial costs two blocking-pool hops (`before_connect`, and the
//! re-check in `after_handshake`) plus one parked watcher task for the connection's life.
//!
//! **No timeout.** Neither hook bounds its store read. A wedged redb store (a stuck fsync, a
//! filesystem hang) therefore stalls EVERY non-pair dial this node makes — gossip's included —
//! until the read returns; each caller's own dial timeout still applies around it. Refusing on a
//! timeout would be the fail-closed choice but would turn a slow disk into a node-wide dial outage,
//! and passing on one would dial a device the store may say is revoked; neither is better than
//! waiting, so it waits.
//!
//! [`dial_refused`]: super::dial::dial_refused
//! [`PeerStore::is_refused`]: crate::allowlist::PeerStore::is_refused
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use iroh::EndpointAddr;
use iroh::endpoint::{
    AfterHandshakeOutcome, BeforeConnectOutcome, Connection, EndpointHooks, Side,
    WeakConnectionHandle,
};
use mcpmesh_net::ALPN_PAIR;

use crate::allowlist::PeerStore;
use crate::roster::gate::RosterGate;

/// The close reason a revoked peer's connection carries, at `after_handshake` and on revoke.
pub(crate) const REVOKED_REASON: &[u8] = b"revoked on this node";

/// The two inputs of [`dial_refused`](super::dial::dial_refused), held without a `MeshState` (which
/// owns the endpoint, so the hook cannot hold it).
#[derive(Clone)]
pub(crate) struct DialGate {
    store: Arc<PeerStore>,
    roster: Arc<RosterGate>,
}

impl DialGate {
    pub(crate) fn new(store: Arc<PeerStore>, roster: Arc<RosterGate>) -> Self {
        Self { store, roster }
    }

    /// [`refused_by`](super::dial::refused_by) on the blocking pool. A read error refuses (inside
    /// `refused_by`); a join failure refuses too.
    ///
    /// The join-failure arm is unpinned by a test: `refused_by` has no panicking path to trigger
    /// without adding a test-only seam into the one revocation predicate, and `util::blocking` —
    /// which maps a panicked task to `Err` — is shared with every other blocking call site.
    async fn refuses(&self, id: [u8; 32]) -> bool {
        let gate = self.clone();
        crate::util::blocking("join dial hook revocation check", move || {
            super::dial::refused_by(&gate.store, gate.roster.view().as_deref(), &id)
        })
        .await
        .unwrap_or(true)
    }
}

/// Every live non-pairing connection this endpoint DIALLED, by remote endpoint id.
///
/// Bounded by live connections: each entry is removed by a watcher on the connection's own close.
#[derive(Default)]
pub(crate) struct PeerConns {
    inner: Mutex<HashMap<[u8; 32], HashMap<u64, WeakConnectionHandle>>>,
    seq: AtomicU64,
}

impl PeerConns {
    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], HashMap<u64, WeakConnectionHandle>>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn insert(&self, id: [u8; 32], weak: WeakConnectionHandle) -> u64 {
        let key = self.seq.fetch_add(1, Ordering::Relaxed);
        self.lock().entry(id).or_default().insert(key, weak);
        key
    }

    fn remove(&self, id: &[u8; 32], key: u64) {
        let mut map = self.lock();
        if let Some(conns) = map.get_mut(id) {
            conns.remove(&key);
            if conns.is_empty() {
                map.remove(id);
            }
        }
    }

    /// Registered connections, all peers.
    pub(crate) fn len(&self) -> usize {
        self.lock().values().map(HashMap::len).sum()
    }

    fn remote_ids(&self) -> Vec<[u8; 32]> {
        self.lock().keys().copied().collect()
    }

    /// Close every registered connection to an id in `ids`; returns how many were closed.
    fn close_ids(&self, ids: &HashSet<[u8; 32]>) -> usize {
        // Upgrade under the lock, close outside it: `close` is cheap, but nothing else runs under
        // this mutex either.
        let conns: Vec<Connection> = self
            .lock()
            .iter()
            .filter(|(id, _)| ids.contains(*id))
            .flat_map(|(_, c)| c.values().filter_map(WeakConnectionHandle::upgrade))
            .collect();
        for c in &conns {
            c.close(mcpmesh_net::CLOSE_UNAUTHORIZED.into(), REVOKED_REASON);
        }
        conns.len()
    }
}

/// Close every registered connection to a device `gate` now refuses. Called by every revoke path,
/// AFTER its write, so a connection registered before the write is found here and one registered
/// after it is refused by its own `after_handshake` re-check. Returns how many were closed.
pub(crate) async fn close_refused(conns: &PeerConns, gate: &DialGate) -> usize {
    let ids = conns.remote_ids();
    if ids.is_empty() {
        return 0;
    }
    // ONE blocking-pool hop for the whole set, not one per peer.
    let g = gate.clone();
    let refused = match crate::util::blocking("join revoke close-pass refusal reads", move || {
        ids.into_iter()
            .filter(|id| super::dial::refused_by(&g.store, g.roster.view().as_deref(), id))
            .collect::<HashSet<_>>()
    })
    .await
    {
        Ok(r) => r,
        // Two failure shapes, deliberately different:
        // - a store READ ERROR is not seen here — `refused_by` answers it as refused (fail closed,
        //   like the gate), so every registered connection that read failed for IS closed. Losing
        //   outbound connections to a device whose revocation state cannot be read is the same
        //   trade the gate makes inbound;
        // - a JOIN failure (the blocking task panicked or was cancelled) closes NOTHING: there is
        //   no answer for any id, and closing all of them would turn one panic into a node-wide
        //   disconnect. New dials to a refused device are still vetoed by `before_connect`.
        Err(e) => {
            tracing::warn!(%e, "revoke close pass failed; connections to refused devices stay open");
            return 0;
        }
    };
    if refused.is_empty() {
        return 0;
    }
    conns.close_ids(&refused)
}

/// Proof that [`MeshHooks::arm`] ran — on SOME instance; it does not name which. Only `arm`
/// constructs it.
pub(crate) struct Armed(());

/// The hooks `build_endpoint` installs. Cloning shares the cell and the registry.
#[derive(Clone, Default)]
pub(crate) struct MeshHooks {
    gate: Arc<OnceLock<DialGate>>,
    conns: Arc<PeerConns>,
}

impl std::fmt::Debug for MeshHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshHooks")
            .field("armed", &self.gate.get().is_some())
            .field("conns", &self.conns.len())
            .finish()
    }
}

impl MeshHooks {
    /// Unarmed: every gated dial is refused until [`arm`](Self::arm).
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Install the gate. Once only; a second call is ignored (and logged), never a swap.
    ///
    /// Returns the [`Armed`] proof the gossip composition requires, so boot cannot subscribe gossip
    /// (and dial its bootstrap set) on an endpoint whose hook still refuses everything.
    pub(crate) fn arm(&self, gate: DialGate) -> Armed {
        if self.gate.set(gate).is_err() {
            tracing::warn!("endpoint dial hook armed twice; keeping the first gate");
        }
        Armed(())
    }

    pub(crate) fn conns(&self) -> Arc<PeerConns> {
        self.conns.clone()
    }

    pub(crate) fn gate(&self) -> Option<DialGate> {
        self.gate.get().cloned()
    }

    /// Unarmed refuses.
    async fn refuses(&self, id: [u8; 32]) -> bool {
        match self.gate.get() {
            Some(gate) => gate.refuses(id).await,
            None => {
                tracing::debug!("dial refused: the endpoint dial hook is not armed yet");
                true
            }
        }
    }
}

impl EndpointHooks for MeshHooks {
    async fn before_connect(
        &self,
        remote_addr: &EndpointAddr,
        alpn: &[u8],
    ) -> BeforeConnectOutcome {
        if alpn == ALPN_PAIR || !self.refuses(*remote_addr.id.as_bytes()).await {
            BeforeConnectOutcome::Accept
        } else {
            BeforeConnectOutcome::Reject
        }
    }

    async fn after_handshake(&self, conn: &Connection) -> AfterHandshakeOutcome {
        // OUTBOUND only, and never pairing. Inbound connections are the accept loop's: its gate
        // refuses them with its own codes and `ConnRegistry` severs them (and COUNTS them, in
        // `severed`). Registering them here too let the close pass cut an inbound session before
        // the inbound sever could count it, so `severed` read 0 for a session it had cut (#229
        // review).
        if conn.side() != Side::Client || conn.alpn() == ALPN_PAIR {
            return AfterHandshakeOutcome::Accept;
        }
        let id = *conn.remote_id().as_bytes();
        let weak = conn.weak_handle();
        // `closed()` is taken NOW, while `conn` is a live strong handle, so the watcher is
        // guaranteed the close event however the connection later ends.
        let closed = weak.closed();
        // REGISTER before the re-check (the TOCTOU close): a revoke whose write lands before
        // the re-check refuses here; one whose write lands after it runs its close pass after
        // this insert and finds the entry.
        let key = self.conns.insert(id, weak);
        let conns = self.conns.clone();
        tokio::spawn(async move {
            closed.await;
            conns.remove(&id, key);
        });
        // A dial that passed `before_connect` and was revoked while its handshake ran.
        if self.refuses(id).await {
            self.conns.remove(&id, key);
            return AfterHandshakeOutcome::Reject {
                error_code: mcpmesh_net::CLOSE_UNAUTHORIZED.into(),
                reason: REVOKED_REASON.to_vec(),
            };
        }
        AfterHandshakeOutcome::Accept
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;
    use std::time::Duration;

    use mcpmesh_net::{ALPN_MCP, ALPN_PAIR, ALPN_PING};

    use super::*;
    use crate::allowlist::RevokedEntry;
    use crate::roster::transport::GOSSIP_ALPN;

    /// Accept counts keyed by (remote id, ALPN).
    type Accepts = Arc<Mutex<HashMap<(iroh::EndpointId, Vec<u8>), usize>>>;

    fn hermetic() -> crate::config::NetworkCfg {
        crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            ..Default::default()
        }
    }

    fn store() -> (Arc<PeerStore>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = PeerStore::open(&tmp.path().join("state.redb")).expect("open store");
        (Arc::new(store), tmp)
    }

    fn revoke(store: &PeerStore, id: iroh::EndpointId) {
        store
            .revoke(RevokedEntry {
                endpoint_id: *id.as_bytes(),
                revoked_at: 1,
                reason: None,
                source: "local".into(),
                signer_user_id: None,
                issued_at: None,
            })
            .expect("revoke");
    }

    /// A plain endpoint (no hooks) that accepts `alpns`, counts each accept by (remote, ALPN), and
    /// HOLDS the connection until the dialer closes it.
    async fn holder(alpns: &[&[u8]]) -> (iroh::Endpoint, Accepts) {
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(alpns.iter().map(|a| a.to_vec()).collect())
            .bind()
            .await
            .expect("bind holder");
        let accepts: Accepts = Arc::default();
        let (ep2, acc) = (ep.clone(), accepts.clone());
        tokio::spawn(async move {
            while let Some(incoming) = ep2.accept().await {
                let acc = acc.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    *acc.lock()
                        .unwrap()
                        .entry((conn.remote_id(), conn.alpn().to_vec()))
                        .or_default() += 1;
                    conn.closed().await;
                });
            }
        });
        (ep, accepts)
    }

    async fn hooked(seed: u8, hooks: &MeshHooks, roster_mode: bool) -> iroh::Endpoint {
        crate::daemon::boot::build_endpoint(
            iroh::SecretKey::from_bytes(&[seed; 32]),
            &hermetic(),
            roster_mode,
            Some(hooks.clone()),
        )
        .await
        .expect("bind hooked endpoint")
    }

    const BEFORE_CONNECT: &str = "rejected by before_connect";
    const AFTER_HANDSHAKE: &str = "rejected by after_handshake";

    async fn dial(
        ep: &iroh::Endpoint,
        to: &iroh::Endpoint,
        alpn: &[u8],
    ) -> Result<Connection, String> {
        use iroh::endpoint::{ConnectError, ConnectWithOptsError, ConnectingError};
        match tokio::time::timeout(Duration::from_secs(10), ep.connect(to.addr(), alpn)).await {
            Ok(Ok(c)) => Ok(c),
            // The two hook refusals print identically; name which hook point refused.
            Ok(Err(ConnectError::Connect {
                source: ConnectWithOptsError::LocallyRejected { .. },
                ..
            })) => Err(BEFORE_CONNECT.into()),
            Ok(Err(ConnectError::Connecting {
                source: ConnectingError::LocallyRejected { .. },
                ..
            })) => Err(AFTER_HANDSHAKE.into()),
            Ok(Err(e)) => Err(format!("{e:?}")),
            Err(_) => Err("timed out".into()),
        }
    }

    /// Before the gate is armed, every gated ALPN fails CLOSED — the dial is refused locally and the
    /// far side never sees a connection — while the pairing ALPN still goes through.
    ///
    /// Mutation: `before_connect` returning `Accept` for an unarmed gate fails the refusal and the
    /// zero count.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unarmed_gate_refuses_every_gated_alpn_but_not_pairing() {
        let hooks = MeshHooks::new();
        let a = hooked(61, &hooks, false).await;
        let (p, accepts) = holder(&[ALPN_MCP, ALPN_PAIR, ALPN_PING, b"app/x/1"]).await;

        for alpn in [ALPN_MCP, ALPN_PING, b"app/x/1".as_slice()] {
            let e = dial(&a, &p, alpn)
                .await
                .expect_err("an unarmed gate must refuse a gated dial");
            assert_eq!(
                e, BEFORE_CONNECT,
                "the refusal must be before_connect's, before any packet — not a transport failure"
            );
        }
        let pair = dial(&a, &p, ALPN_PAIR)
            .await
            .expect("the pairing ALPN is not gated, armed or not");
        pair.close(0u32.into(), b"done");

        tokio::time::sleep(Duration::from_millis(300)).await;
        let acc = accepts.lock().unwrap().clone();
        assert_eq!(
            acc.get(&(a.id(), ALPN_PAIR.to_vec())),
            Some(&1),
            "control: the pair dial reached the holder: {acc:?}"
        );
        assert_eq!(acc.len(), 1, "no gated ALPN may reach the holder: {acc:?}");
    }

    /// An ARMED gate refuses a revoked id on every non-pair ALPN and accepts an unrevoked one.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_armed_gate_refuses_a_revoked_id_and_admits_others() {
        let (store, _tmp) = store();
        let hooks = MeshHooks::new();
        hooks.arm(DialGate::new(store.clone(), Arc::new(RosterGate::empty())));
        let a = hooked(62, &hooks, false).await;
        let alpns: [&[u8]; 4] = [ALPN_MCP, ALPN_PAIR, ALPN_PING, b"app/x/1"];
        let (bad, bad_acc) = holder(&alpns).await;
        let (good, good_acc) = holder(&alpns).await;
        revoke(&store, bad.id());

        for alpn in [ALPN_MCP, ALPN_PING, b"app/x/1".as_slice()] {
            let e = dial(&a, &bad, alpn)
                .await
                .expect_err("a revoked id must be refused");
            assert_eq!(e, BEFORE_CONNECT);
            let c = dial(&a, &good, alpn)
                .await
                .expect("an unrevoked id is dialled");
            c.close(0u32.into(), b"done");
        }
        dial(&a, &bad, ALPN_PAIR)
            .await
            .expect("pairing stays exempt; its own dial paths check revocation")
            .close(0u32.into(), b"done");

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            bad_acc.lock().unwrap().len(),
            1,
            "only the pair dial may reach the revoked id"
        );
        assert_eq!(
            good_acc.lock().unwrap().len(),
            3,
            "control: the unrevoked id is reached on all three gated ALPNs"
        );
    }

    /// A dial that passed `before_connect` and was revoked before its handshake completed is
    /// rejected at `after_handshake`. A second hook installed AFTER ours revokes the target inside
    /// its own `before_connect`, which deterministically lands the revoke in that window.
    ///
    /// Mutation: dropping the outbound re-check in `after_handshake` returns the connection.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoke_landing_mid_dial_is_rejected_at_handshake() {
        struct RevokeOnDial(Arc<PeerStore>);
        impl std::fmt::Debug for RevokeOnDial {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("RevokeOnDial")
            }
        }
        impl EndpointHooks for RevokeOnDial {
            fn before_connect<'a>(
                &'a self,
                remote: &'a EndpointAddr,
                _alpn: &'a [u8],
            ) -> impl Future<Output = BeforeConnectOutcome> + Send + 'a {
                revoke(&self.0, remote.id);
                async { BeforeConnectOutcome::Accept }
            }
        }

        let (store, _tmp) = store();
        let hooks = MeshHooks::new();
        hooks.arm(DialGate::new(store.clone(), Arc::new(RosterGate::empty())));
        let a = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .hooks(hooks.clone())
            .hooks(RevokeOnDial(store.clone()))
            .bind()
            .await
            .unwrap();
        let (p, _acc) = holder(&[ALPN_MCP]).await;
        let e = dial(&a, &p, ALPN_MCP)
            .await
            .expect_err("a dial revoked mid-flight must not be handed back");
        assert_eq!(
            e, AFTER_HANDSHAKE,
            "the re-check at the handshake must refuse it"
        );
        let conns = hooks.conns();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while conns.len() != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a rejected connection must not stay registered"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Every OUTBOUND non-pair connection is registered, inbound and pairing ones are not, and the
    /// registry drains once the connections close — bounded memory.
    ///
    /// Mutations: removing the close watcher leaves entries behind; removing the registration fails
    /// the "while open" count; registering inbound connections fails the inbound count.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_registry_tracks_outbound_connections_and_drains_when_they_close() {
        let (store, _tmp) = store();
        let hooks = MeshHooks::new();
        hooks.arm(DialGate::new(store, Arc::new(RosterGate::empty())));
        let a = hooked(63, &hooks, false).await;
        let (p, _acc) = holder(&[ALPN_MCP, ALPN_PAIR]).await;

        const N: usize = 8;
        let mut open = Vec::new();
        for _ in 0..N {
            open.push(dial(&a, &p, ALPN_MCP).await.expect("dial"));
        }
        let pair = dial(&a, &p, ALPN_PAIR).await.expect("pair dial");
        assert_eq!(
            hooks.conns().len(),
            N,
            "each outbound non-pair connection is registered; the pair one is not"
        );

        // Inbound: a plain endpoint dials INTO `a`. The accept loop's `ConnRegistry` owns inbound
        // connections, so this registry must NOT count it — registering it here too made `severed`
        // undercount (#229 review).
        let (a2, _) = (a.clone(), ());
        let accepted = Arc::new(tokio::sync::Notify::new());
        let acc2 = accepted.clone();
        tokio::spawn(async move {
            while let Some(inc) = a2.accept().await {
                if let Ok(c) = inc.await {
                    acc2.notify_one();
                    c.closed().await;
                }
            }
        });
        let back = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let inbound = dial(&back, &a, ALPN_MCP).await.expect("inbound dial");
        tokio::time::timeout(Duration::from_secs(10), accepted.notified())
            .await
            .expect("control: `a` completed the inbound handshake (after_handshake ran)");
        assert_eq!(
            hooks.conns().len(),
            N,
            "an INBOUND connection must not be registered — only the {N} outbound ones"
        );

        for c in open.drain(..) {
            c.close(0u32.into(), b"done");
        }
        pair.close(0u32.into(), b"done");
        drop(inbound);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while hooks.conns().len() != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the registry must drain once its connections close, holding {}",
                hooks.conns().len()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Gossip participant used by the learned-peer test: an unhooked endpoint (optionally refusing
    /// to dial `never_dial`) running iroh-gossip, counting accepts by (remote, ALPN), and tracking
    /// every neighbour it ever had on `topic`.
    struct Participant {
        ep: iroh::Endpoint,
        accepts: Accepts,
    }

    #[derive(Debug)]
    struct NeverDial(iroh::EndpointId);
    impl EndpointHooks for NeverDial {
        fn before_connect<'a>(
            &'a self,
            remote: &'a EndpointAddr,
            _alpn: &'a [u8],
        ) -> impl Future<Output = BeforeConnectOutcome> + Send + 'a {
            let refuse = remote.id == self.0;
            async move {
                if refuse {
                    BeforeConnectOutcome::Reject
                } else {
                    BeforeConnectOutcome::Accept
                }
            }
        }
    }

    /// Run gossip on `ep`: an accept loop counting (remote, ALPN) and handing gossip connections to
    /// the gossip handler.
    fn serve_gossip(ep: &iroh::Endpoint) -> (iroh_gossip::net::Gossip, Accepts) {
        let gossip = crate::roster::transport::spawn_gossip(ep);
        let accepts: Accepts = Arc::default();
        let (ep2, g2, acc) = (ep.clone(), gossip.clone(), accepts.clone());
        tokio::spawn(async move {
            while let Some(inc) = ep2.accept().await {
                let (g, acc) = (g2.clone(), acc.clone());
                tokio::spawn(async move {
                    let Ok(conn) = inc.await else { return };
                    *acc.lock()
                        .unwrap()
                        .entry((conn.remote_id(), conn.alpn().to_vec()))
                        .or_default() += 1;
                    if conn.alpn() == GOSSIP_ALPN {
                        let _ = g.handle_connection(conn).await;
                    }
                });
            }
        });
        (gossip, accepts)
    }

    /// Subscribe and record every neighbour ever reported, plus the live set.
    async fn join(
        gossip: &iroh_gossip::net::Gossip,
        topic: [u8; 32],
        bootstrap: Vec<iroh::EndpointId>,
    ) -> (
        iroh_gossip::api::GossipSender,
        Arc<Mutex<HashSet<iroh::EndpointId>>>,
    ) {
        use n0_future::StreamExt as _;
        let rg = crate::roster::transport::subscribe(gossip, topic, bootstrap)
            .await
            .expect("subscribe");
        let ever: Arc<Mutex<HashSet<iroh::EndpointId>>> = Arc::default();
        let mut rx = rg.receiver.expect("receiver");
        let e2 = ever.clone();
        tokio::spawn(async move {
            while let Some(Ok(ev)) = rx.next().await {
                if let iroh_gossip::api::Event::NeighborUp(id) = ev {
                    e2.lock().unwrap().insert(id);
                }
            }
        });
        (rg.sender, ever)
    }

    fn seed(ep: &iroh::Endpoint, peers: &[&iroh::Endpoint]) {
        let mem = iroh::address_lookup::MemoryLookup::new();
        for p in peers {
            mem.add_endpoint_info(p.addr());
        }
        ep.address_lookup().expect("lookup").add(mem);
    }

    async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// #229 §1: a revoked device learned through the gossip SWARM is never dialled.
    ///
    /// `a` is a hooked endpoint from `build_endpoint` with `x` revoked in its store. `b` is an
    /// unhooked member that has not revoked anyone. `x` joins through `b`; with `a`'s active view at
    /// one peer, HyParView's ForwardJoin makes `a` send `x` a Neighbor request, i.e. dial it
    /// (iroh-gossip `hyparview.rs:412`, `net.rs` `Dialer::queue_dial`). `a` is then told to join `x`
    /// directly too. `x` never dials `a` (its own test hook refuses), so any `a`↔`x` connection
    /// would have to be `a`'s dial: `x` must count ZERO accepts from `a` on every ALPN, and `x` must
    /// never be `a`'s neighbour.
    ///
    /// The CONTROL is `y`, unrevoked, joining the same way afterwards: `a` dials it and it becomes a
    /// neighbour, so the zero for `x` cannot be a swarm that never forwarded anything.
    ///
    /// Mutation: removing `builder.hooks(h)` from `build_endpoint` fails both `x` assertions.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_peer_learned_through_gossip_is_never_dialled() {
        let topic = *blake3::hash(b"mcpmesh/test/229").as_bytes();
        let (store, _tmp) = store();
        let hooks = MeshHooks::new();
        hooks.arm(DialGate::new(store.clone(), Arc::new(RosterGate::empty())));
        let a = hooked(64, &hooks, true).await;

        let plain = |alpns: Vec<Vec<u8>>| async move {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(alpns)
                .bind()
                .await
                .unwrap()
        };
        let b = plain(vec![GOSSIP_ALPN.to_vec()]).await;
        let y = plain(vec![GOSSIP_ALPN.to_vec()]).await;
        let x = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(crate::daemon::boot::alpns_for(true))
            .hooks(NeverDial(a.id()))
            .bind()
            .await
            .unwrap();
        revoke(&store, x.id());
        for (ep, others) in [
            (&a, [&b, &x, &y]),
            (&b, [&a, &x, &y]),
            (&x, [&a, &b, &y]),
            (&y, [&a, &b, &x]),
        ] {
            seed(ep, &others);
        }
        let (ga, _a_acc) = serve_gossip(&a);
        let (gb, _b_acc) = serve_gossip(&b);
        let (gx, x_acc) = serve_gossip(&x);
        let (gy, y_acc) = serve_gossip(&y);
        let x_p = Participant {
            ep: x.clone(),
            accepts: x_acc,
        };

        let (_sb, b_ever) = join(&gb, topic, vec![]).await;
        let (sa, a_ever) = join(&ga, topic, vec![b.id()]).await;
        wait_until("a and b become neighbours", || {
            a_ever.lock().unwrap().contains(&b.id())
        })
        .await;

        let x_from_a = || -> Vec<(String, usize)> {
            x_p.accepts
                .lock()
                .unwrap()
                .iter()
                .filter(|((r, _), _)| *r == a.id())
                .map(|((_, alpn), n)| (String::from_utf8_lossy(alpn).into_owned(), *n))
                .collect()
        };

        // (1) ForwardJoin: x joins through b while a's active view is exactly {b}.
        let (_sx, _x_ever) = join(&gx, topic, vec![b.id()]).await;
        wait_until("b admits x", || b_ever.lock().unwrap().contains(&x.id())).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            x_from_a().is_empty(),
            "a must never dial a revoked device it learned through a ForwardJoin, on any ALPN: \
             {:?}",
            x_from_a()
        );

        // (2) An explicit join of x (the same Dialer path a Shuffle-learned peer takes).
        sa.join_peers(vec![x.id()]).await.expect("join_peers x");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            x_from_a().is_empty(),
            "a must never dial a revoked device it is told to join, on any ALPN: {:?}",
            x_from_a()
        );
        assert!(
            !a_ever.lock().unwrap().contains(&x_p.ep.id()),
            "a revoked device must never become a gossip neighbour"
        );

        // The CONTROL: the same explicit join for unrevoked y DOES dial it and make it a
        // neighbour, so the zeros above are the hook's doing, not a swarm that never dialled.
        let (_sy, _y_ever) = join(&gy, topic, vec![]).await;
        sa.join_peers(vec![y.id()]).await.expect("join_peers y");
        wait_until("control: a dials y and makes it a neighbour", || {
            a_ever.lock().unwrap().contains(&y.id())
                && y_acc
                    .lock()
                    .unwrap()
                    .contains_key(&(a.id(), GOSSIP_ALPN.to_vec()))
        })
        .await;
    }

    /// Mint a roster view (serial `serial`) with one user holding device `dev`, revoking `revoked`.
    fn roster_view(
        serial: u64,
        dev: [u8; 32],
        revoked: &[[u8; 32]],
    ) -> mcpmesh_trust::roster::validate::RosterView {
        use mcpmesh_trust::roster::sign::mint_signed;
        use mcpmesh_trust::roster::validate::load_installed;
        use mcpmesh_trust::roster::{Roster, RosterDevice, RosterUser, encode_b64u};
        let root = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let signed = mint_signed(
            &root,
            Roster {
                format: "mcpmesh-roster/1".into(),
                org_id: "acme".into(),
                serial,
                issued_at: "2000-01-01T00:00:00Z".into(),
                expires_at: "2999-01-01T00:00:00Z".into(),
                groups: vec!["team".into()],
                users: vec![RosterUser {
                    user_id: "alice".into(),
                    display_name: "alice".into(),
                    user_pk: encode_b64u(&[1u8; 32]),
                    groups: vec!["team".into()],
                    devices: vec![RosterDevice {
                        endpoint_id: encode_b64u(&dev),
                        label: "laptop".into(),
                        role: "primary".into(),
                    }],
                }],
                revoked_endpoints: revoked.iter().map(|e| encode_b64u(e)).collect(),
                successor_root_pk: None,
                successor_sig: None,
                sig: String::new(),
            },
        );
        load_installed(&signed, &root.verifying_key()).expect("valid roster view")
    }

    /// #229: a ROSTER install that revokes a device closes this node's OUTBOUND connections to it.
    ///
    /// Through a really booted node (so the hooks, the arm and the mesh wiring are boot's), holding
    /// an app-protocol connection it dialled to `p`. Installing a roster whose `revoked_endpoints`
    /// names `p` must close that connection within a bounded wait; a roster that does NOT revoke `p`
    /// (the control, installed first) must leave it open.
    ///
    /// Mutation: removing the close pass from `install_roster_view_and_sever` fails the wait.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_roster_install_that_revokes_a_device_closes_our_outbound_connection_to_it() {
        let root = tempfile::tempdir().unwrap();
        let cfg =
            crate::config::Config::from_toml_str("[network]\nrelay_mode = \"disabled\"\n").unwrap();
        let booted = crate::daemon::boot::start_node(
            crate::paths::NodePaths::under_root(root.path()),
            Some(cfg),
            Default::default(),
        )
        .await
        .expect("boot");
        let mesh = booted.state.mesh().expect("mesh").clone();
        let (p, _acc) = holder(&[b"app/hold/1"]).await;

        let conn = dial(&mesh.endpoint, &p, b"app/hold/1")
            .await
            .expect("an unrevoked device is dialled");

        crate::daemon::install_roster_view_and_sever(
            &mesh,
            roster_view(1, *p.id().as_bytes(), &[]),
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            conn.close_reason().is_none(),
            "control: a roster that does not revoke the device leaves the connection open"
        );

        crate::daemon::install_roster_view_and_sever(
            &mesh,
            roster_view(2, [3u8; 32], &[*p.id().as_bytes()]),
        );
        let reason = tokio::time::timeout(Duration::from_secs(10), conn.closed())
            .await
            .expect("our outbound connection to a device the roster revoked must close within 10s");
        assert!(
            matches!(reason, iroh::endpoint::ConnectionError::LocallyClosed),
            "this node must have closed it: {reason:?}"
        );
        crate::daemon::boot::shutdown_booted(booted).await;
    }

    /// The hooks never keep their endpoint alive (iroh `hooks.rs:60-64`: a hook holding the
    /// `Endpoint` is a reference cycle). Observed through the store `Arc` the armed gate holds:
    /// once the endpoint is closed and dropped — with connections that were registered, watched and
    /// closed — the endpoint's copy of the hooks must be dropped too, returning the count to ours.
    ///
    /// Mutation: stashing an `Endpoint` clone in `MeshHooks`, or a strong `Connection` in the
    /// registry, keeps the count up and fails the wait.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_hooks_do_not_keep_their_endpoint_alive() {
        let (store, _tmp) = store();
        let hooks = MeshHooks::new();
        let _armed = hooks.arm(DialGate::new(store.clone(), Arc::new(RosterGate::empty())));
        let a = hooked(65, &hooks, false).await;
        let (p, _acc) = holder(&[ALPN_MCP]).await;
        let conn = dial(&a, &p, ALPN_MCP).await.expect("dial");
        assert_eq!(
            hooks.conns().len(),
            1,
            "control: the connection was registered"
        );
        drop(hooks);
        assert_eq!(
            Arc::strong_count(&store),
            2,
            "control: the endpoint's hooks hold the only other reference"
        );

        conn.close(0u32.into(), b"done");
        drop(conn);
        a.close().await;
        drop(a);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&store) != 1 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a closed, dropped endpoint must release its hooks (strong count {})",
                Arc::strong_count(&store)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
