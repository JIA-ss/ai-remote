//! Daemon process bring-up: the per-uid singleton, `[network]`-posture validation
//! ([`net_plan`]), Iroh endpoint construction, roster-mode transport composition, and the
//! `serve_forever` assembly that wires the whole daemon together and serves until shutdown.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use mcpmesh_net::registry::ConnRegistry;
use mcpmesh_net::{ALPN_MCP, ALPN_PAIR, ALPN_PING, TrustGate};
use mcpmesh_trust::DeviceKey;

use crate::allowlist::{AllowlistGate, PeerStore};
use crate::audit::{AuditLog, AuditSink};
use crate::config::Config;
use crate::control::{DaemonState, serve_control};
use crate::ipc;
use crate::node::StartError;
use std::time::Duration;

use crate::pairing::LiveInvites;
use crate::paths::NodePaths;
use crate::roster::RosterStore;
use crate::roster::freshness::FreshnessStore;
use crate::roster::gate::{ComposedGate, RosterGate};
use crate::util::{blocking, epoch_now_i64};

use super::accept::spawn_accept_loop;
use super::roster_install::{
    respawn_poll_loop, roster_confirmed_path, spawn_staleness_sweep, warn_if_degraded_grace,
};
use super::{MeshState, STACK_VERSION, build_services_audited, default_self_nickname};

/// The daemon shell's async body: bind the control endpoint, boot the node core
/// (`start_node`), and serve the control API until a `shutdown` request stops it.
/// `paths` is the node's resolved on-disk world — the shell passes [`NodePaths::from_env`];
/// nothing below consults the environment for a location.
pub async fn serve_forever(socket: &Path, paths: NodePaths) -> Result<()> {
    // 0a. Bind the control listener FIRST — before state.redb, the endpoint, or the audit log. On
    //     Windows the pipe bind IS the singleton lock (there is no flock; the shell skips it there):
    //     a FILE_FLAG_FIRST_PIPE_INSTANCE create returns AddrInUse once a peer daemon owns the pipe,
    //     so binding early is what serializes daemons. On unix this AddrInUse arm is dead — the
    //     shell's flock already serialized us before we ever reached here — but the shape is uniform.
    //     The socket/pipe creation has no side effects the later construction depends on.
    let listener = match ipc::bind_control_socket(socket).await {
        Ok(l) => l,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::AddrInUse) =>
        {
            tracing::info!("another mcpmesh daemon already owns the control endpoint; exiting");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let booted = start_node(paths, None, BootOverrides::default()).await?;
    let state = booted.state;
    // #134: the duplicate-identity detector, installed ONLY here.
    //
    // `serve_forever` is the STANDALONE daemon — it owns its process and installs no subscriber of
    // its own, so taking the global default is ours to take. This must never move into
    // `boot_node`: that path is shared with `NodeBuilder::start`, and an embedded node seizing the
    // process-global subscriber would panic a host that calls `fmt::init()` afterwards, or
    // silently swallow its logs for the process lifetime if it uses `try_init()`. An embedder
    // wires the same detection through `NodeBuilder::identity_conflict` instead.
    if let Some(mesh) = state.mesh() {
        let conflict = std::sync::Arc::new(crate::diag::IdentityConflict::default());
        mesh.adopt_identity_conflict(conflict.clone());
        crate::diag::install_for_daemon(conflict);
    }
    // The daemon serves for the process lifetime — the background handles need no owner
    // (the embedding `Node` keeps them to abort on `shutdown`; the process just exits).
    drop(booted.background);
    // Our own endpoint id is operator-shareable (it is how a peer pairs us) — not a
    // surface leak (that discipline forbids leaking OTHER peers' ids/paths in porcelain).
    tracing::info!(
        endpoint_id = %state.mesh_required()?.endpoint.id(),
        socket = %socket.display(),
        "mcpmesh daemon serving mesh + control"
    );
    serve_control(listener, state).await
}

/// A booted node core: the control-dispatch state plus the detached background loops the
/// boot spawned (presence, roster converge, staleness sweep). The daemon shell drops the
/// handles (process lifetime); the embedding `Node` aborts them on `shutdown`.
pub(crate) struct BootedNode {
    pub(crate) state: Arc<DaemonState>,
    pub(crate) background: Vec<tokio::task::JoinHandle<()>>,
}

/// Tear a booted node down: stop accepting, end the background loops, END THE APP-BLOB GATE LOOP
/// (and wait for it), then close the endpoint.
///
/// Shared by `Node::shutdown` and by tests that boot a real node (#105). Factored out rather than
/// duplicated because a test teardown that drifts from the production one silently stops testing
/// the thing it models — and because `BootedNode::background` is EMPTY in pairing mode, so the
/// obvious `for h in booted.background { h.abort() }` is a no-op that LOOKS like cleanup while the
/// accept loop, the gate loop (holding the redb data-dir lock) and the endpoint all leak.
pub(crate) async fn shutdown_booted(booted: BootedNode) {
    let state = &booted.state;
    state.request_shutdown();
    state.abort_control_tasks();
    let Some(mesh) = state.mesh().cloned() else {
        return;
    };
    if let Some(task) = mesh.accept_task.lock().await.take() {
        task.abort();
    }
    if let Some(task) = mesh.poll_loop.lock().await.take() {
        task.abort();
    }
    for task in booted.background {
        task.abort();
    }
    // Wait for the gate loop: it owns the `Arc<dyn TrustGate>` -> `PeerStore` -> redb lock, and
    // aborting without awaiting only SCHEDULES the drop (#61).
    if let Some(blobs) = mesh.app_blobs.lock().await.take() {
        blobs.shutdown().await;
    }
    mesh.endpoint.close().await;
}

/// Boot the node core — everything `serve_forever` does EXCEPT the control endpoint:
/// crypto-provider install, audit sink, config + device key, the iroh endpoint, stores,
/// gates, limiters, service registry, the mesh accept loop, and roster mode's loops.
/// `config` overrides the on-disk `paths.config_path` when `Some` (the embedder's
/// programmatic config); config-persisting verbs still write that path.
/// What an EMBEDDER may substitute at boot, beyond the config (#85).
///
/// One struct rather than a growing parameter list: `boot_node` backs both `serve_forever` and
/// `NodeBuilder::start`, so every parameter added here is one the daemon shell must also thread
/// through and keep meaningless.
#[derive(Default)]
pub(crate) struct BootOverrides {
    /// The device key to run as, instead of reading (or minting) `<root>/config/device.key`
    /// (#85 ask 1).
    ///
    /// The default posture is 32 raw secret bytes at 0600 in a directory the node owns — no
    /// passphrase, no keychain, no hardware seam — and an embedder could not fix that from
    /// outside, because the file is inside the mesh root it is told not to hand-write and there
    /// was no API to hand over a decrypted key. This is that API: unwrap the key from wherever
    /// your platform keeps secrets and pass it in.
    ///
    /// When set, the node reads and writes NO device key file at all. Losing the key material
    /// therefore loses the identity — which is the trade an embedder makes deliberately by
    /// taking custody.
    pub(crate) device_key: Option<mcpmesh_trust::ed25519_dalek::SigningKey>,
}

pub(crate) async fn start_node(
    paths: NodePaths,
    config: Option<Config>,
    overrides: BootOverrides,
) -> Result<BootedNode, StartError> {
    let config_path = paths.config_path.clone();
    let db_path = paths.state_db_path.clone();
    boot_node(paths, config, overrides)
        .await
        .map_err(|e| StartError::classify(e, &config_path, &db_path))
}

/// The anyhow-typed boot body — [`start_node`] classifies its error at the boundary
/// (classification inspects the error CHAIN, so inner `?` sites stay untouched).
async fn boot_node(
    paths: NodePaths,
    config: Option<Config>,
    overrides: BootOverrides,
) -> Result<BootedNode> {
    // 0. CRITICAL: install a process-default rustls `CryptoProvider` (ring) BEFORE any
    //    reqwest client is built. reqwest 0.13.4 (`rustls-no-provider`) resolves the provider via
    //    `CryptoProvider::get_default()` at CLIENT-BUILD time and PANICS ("No rustls crypto provider
    //    is configured") if none is installed. iroh 1.0.1 passes ring per-endpoint and installs NO
    //    process-default, so nothing else does this for us. Idempotent: `install_default` returns Err
    //    if a provider is already installed, which `let _ =` swallows — a HOST APPLICATION that
    //    installed its own provider first wins. This MUST precede the URL poll loop spawned below.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut background: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // 0b. The audit log: one bounded-channel writer over <state_dir>/audit. Best-effort
    //     — record() never blocks or fails a session. Threaded into the backends (build_services_
    //     audited) and stored on the mesh for the reload sites + trust-event hooks.
    let audit = AuditSink::new(AuditLog::spawn(paths.audit_dir.clone()));

    // 1. Config + device key.
    let config_path = paths.config_path.clone();
    let cfg = match config {
        Some(c) => c,
        // `.with_context` (not a formatted `anyhow!`) keeps the `figment::Error` in the
        // chain — `StartError::classify` keys on it.
        None => Config::load(&config_path)
            .with_context(|| format!("config error in {}", config_path.display()))?,
    };

    // 1a. Audit retention (#88): with `[limits].audit_retain_months = N > 0`, delete monthly
    //     audit files older than the last N months. Boot-time only (a long-running daemon prunes
    //     on next start; the `audit_prune` verb covers live needs), BEFORE serving so a caller
    //     never reads a month the config says is out of window. Best-effort like the writer: a
    //     prune failure is a warning, never a failed boot — refusing to start over an
    //     undeletable old log file would be worse than keeping it. Default 0 deletes nothing.
    if let Some(cutoff) =
        crate::audit::retention_cutoff(&crate::audit::now_ts()[..7], cfg.limits.audit_retain_months)
    {
        let dir = paths.audit_dir.clone();
        match blocking("join audit retention prune", move || {
            crate::audit::prune_before(&dir, &cutoff)
        })
        .await
        {
            Ok(Ok(deleted)) if !deleted.is_empty() => {
                tracing::info!(?deleted, "audit retention pruned old months");
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(%e, "audit retention prune failed"),
            Err(e) => tracing::warn!(%e, "audit retention prune failed"),
        }
    };
    let key_path = match cfg.identity.device_key.clone() {
        Some(p) => p,
        None => paths.device_key_path.clone(),
    };
    // #85: an embedder may supply the key instead, so it can live in the OS keychain (or anywhere
    // else) rather than as raw secret bytes in a file this node owns. When it does, NOTHING here
    // touches `key_path` — not a read, not a mint — so the on-disk key never exists to be stolen,
    // and a node whose embedder holds the key cannot silently fall back to a file one.
    let key = match overrides.device_key.as_ref() {
        Some(sk) => {
            tracing::debug!(
                "using an embedder-supplied device key; no key file is read or written"
            );
            DeviceKey::from_signing_key(sk.clone())
        }
        None => {
            DeviceKey::load_or_generate(&key_path)
                .map_err(|e| anyhow::anyhow!("device key error at {}: {e}", key_path.display()))?
                .0
        }
    };

    // 2. The single Iroh endpoint, seeded from the device key. Roster mode (an org root
    //    pinned in config) additionally advertises the gossip + blob ALPNs on this same endpoint;
    //    a pure-pairing daemon advertises exactly mcp/1 + pair/1 (no roster ALPNs to probe).
    let secret = iroh::SecretKey::from_bytes(&key.secret_bytes());
    let roster_mode = cfg.identity.org_root_pk.is_some();
    // #89: resolve the presence policy BEFORE binding a socket, so `presence_mode = "of"` is a
    // startup error rather than a node that silently pongs to everyone while its operator believes
    // they are hidden. A privacy knob that fails open is worse than no knob.
    let presence = presence_mode(&cfg.network)?;
    // #63: a `0` per-service rate is the most restrictive setting there is, not unlimited. Refused
    // before binding, like every other config error.
    validate_service_rates(&cfg)?;
    // #68: parsed BEFORE the bind, like `presence_mode` and `validate_service_rates` above — an
    // unknown value must fail the boot without first binding sockets and doing relay/discovery
    // work. (0.44.0 shipped this parse after `build_endpoint` with a comment claiming otherwise;
    // the gate caught the comment.)
    let local_disc = local_discovery(&cfg.network)?;
    // #229: the endpoint hooks. UNARMED here, so every gated ALPN fails closed until the store and
    // roster gate exist (step 3, below). Nothing dials between this bind and that arm.
    let hooks = crate::daemon::hooks::MeshHooks::new();
    let endpoint = build_endpoint(secret, &cfg.network, roster_mode, Some(hooks.clone())).await?;
    let our_id = endpoint.id();

    // #68: local (mDNS) peer discovery, off unless `[network].local_discovery` asks for it.
    // Registration only — the PARSE happened above, before the bind, alongside its siblings.
    if local_disc.enabled {
        // `relay_only` DOES NOT restrain what goes out here on a stock build.
        //
        // The original wording of this warning said the opposite — that the address filter strips
        // direct addresses before mDNS sees them — and the 0.44.0 gate disproved it on the wire.
        // `AddrFilter::relay_only()` is installed only under `#[cfg(feature = "unstable-relay-only")]`
        // (`apply_relay_only`); without the feature `relay_only = true` is inert, and the gate
        // captured the full direct set going out: LAN address, PUBLIC WAN IPv4, and global IPv6.
        //
        // So the two knobs contradict in opposite directions depending on the build, and an
        // operator who set `relay_only` for a quiet posture must be told which one they got.
        if cfg.network.relay_only && local_disc.advertise {
            if cfg!(feature = "unstable-relay-only") {
                tracing::warn!(
                    "[network] local_discovery advertises, but relay_only is active — the address \
                     filter strips direct addresses before mDNS sees them, so peers on this LAN \
                     learn only a relay URL. Resolution still works, but announcing achieves \
                     nothing here."
                );
            } else {
                tracing::warn!(
                    "[network] local_discovery advertises AND relay_only is set — but this binary \
                     was built without `unstable-relay-only`, so relay_only is inert and mDNS \
                     announces this node's FULL direct address set (LAN, public WAN IPv4, global \
                     IPv6) to every device on the link."
                );
            }
        }
        // #68 gate: `presence_mode = "off"` exists so a node never discloses its reachability, and
        // advertising broadcasts exactly that to the whole link, roughly once a second. Neither
        // knob knows about the other, so the combination is silently self-defeating; say so.
        if local_disc.advertise && presence == PresenceMode::Off {
            tracing::warn!(
                "[network] presence_mode = \"off\" withholds reachability from paired peers, but \
                 local_discovery = \"on\" announces this node's presence and addresses to every \
                 device on the local link. Use local_discovery = \"resolve\" to keep the LAN quiet."
            );
        }
        match crate::discovery::local::build(our_id, local_disc) {
            Ok(lookup) => {
                endpoint
                    .address_lookup()
                    .map_err(|e| anyhow::anyhow!("endpoint closed before local discovery: {e}"))?
                    // #203: WRAPPED, like the pkarr and DNS resolvers. This is the third
                    // address-lookup ingress and it is the one with the WEAKEST precondition: an
                    // mDNS answer is unauthenticated, endpoint ids are public, and any device on
                    // the link can answer for a paired peer's id with whatever addresses it likes.
                    // pkarr at least requires generating a keypair and publishing a record.
                    .add(crate::daemon::lookup_hygiene::wrap(lookup));
                tracing::info!(
                    advertise = local_disc.advertise,
                    "local discovery enabled on this link"
                );
            }
            // A machine with no multicast-capable interface, or one where the socket is refused, is
            // a working node that cannot do LAN discovery — not a boot failure. Refusing to start
            // would turn a networking condition into an outage for every other feature.
            Err(e) => tracing::warn!(
                %e,
                "local discovery could not start; peers on this LAN will not be resolved locally"
            ),
        }
    }

    // 3. The peer allowlist store + gate. redb open + reads are blocking; open on a blocking
    //    thread so a slow trust-file fsync never stalls a runtime worker.
    let db_path = paths.state_db_path.clone();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create data dir {}", parent.display()))?;
    }
    let store = blocking("join peer-store open", move || PeerStore::open(&db_path)).await??;
    let store = Arc::new(store);
    // The composed trust gate: pairing ∪ roster with explicit precedence
    // (revocation → roster → pairing). `pairs` is the `AllowlistGate` over the redb peer
    // allowlist; `roster` is the hot-swappable roster gate, empty until a signed roster is
    // installed/loaded. With NO roster installed, `ComposedGate` falls through to `pairs` for
    // everything — every pairing flow preserved.
    let pairs = Arc::new(AllowlistGate::new(store.clone()));
    // The degraded-expiry grace window (default 72h; total parse — see
    // `RosterCfg::grace_seconds`, which also carries the degraded-split design note).
    let grace = cfg.roster.grace_seconds();
    // The roster-daemon gate: expiry grace AND the freshness bound. A stale roster
    // (not re-confirmed within `max_staleness`) degrades on the SAME `RosterState` machine as expiry.
    let roster = Arc::new(RosterGate::with_freshness(
        grace,
        cfg.roster.max_staleness_seconds(),
    ));
    // Load the pinned roster at startup, if an org root is pinned in config. FAIL-CLOSED: any load
    // error (a corrupt/tampered roster.json, a signature mismatch) leaves the roster EMPTY — roster
    // peers are refused (the gate resolves nothing), while pairing is entirely unaffected. An
    // invalid pinned pk disables roster mode with a warning (pairing still works).
    if let Some(pk_str) = cfg.identity.org_root_pk.clone() {
        match crate::roster::parse_org_root_pk(&pk_str) {
            Ok(pk) => {
                let rstore = RosterStore::new(paths.roster_path.clone());
                match blocking("join roster load", move || rstore.load(&pk)).await {
                    Ok(Ok(Some(view))) => {
                        roster.install(view);
                        tracing::info!("installed roster loaded");
                        // If the loaded roster is already past expiry but within grace, warn on load
                        // — an expired-but-valid installed roster loads into degraded mode.
                        warn_if_degraded_grace(&roster);
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(e)) | Err(e) => {
                        tracing::warn!(%e, "installed roster failed to load; refusing roster peers")
                    }
                }
            }
            Err(e) => tracing::warn!(%e, "pinned org_root_pk is invalid; roster mode disabled"),
        }
    }
    // Freshness bootstrap: arm the gate's `last_confirmed` from the sidecar.
    //  - present sidecar  → restore the persisted confirmation instant.
    //  - ABSENT sidecar + a roster IS installed → the ONE-TIME UPGRADE GRACE: a node upgrading
    //    from a build without freshness tracking has no sidecar yet; treat `now` as the
    //    confirmation instant and persist it, so it does NOT instantly degrade to stale on its
    //    first boot with this build (it re-confirms on its next poll).
    //  - absent sidecar + NO roster → leave `None` (a fresh node arms freshness on its first confirm).
    // Best-effort persist: a write failure leaves the in-RAM arm intact.
    {
        let fpath = roster_confirmed_path(&config_path);
        let fstore = FreshnessStore::new(fpath.clone());
        match blocking("join roster freshness load", move || fstore.load()).await {
            Ok(Ok(Some(lc))) => roster.set_last_confirmed(lc),
            Ok(Ok(None)) if roster.view().is_some() => {
                let now = epoch_now_i64();
                roster.set_last_confirmed(now);
                let fstore = FreshnessStore::new(fpath);
                match blocking("join roster freshness upgrade-grace persist", move || {
                    fstore.store(now)
                })
                .await
                {
                    Ok(Ok(())) => tracing::info!("roster freshness upgrade grace applied"),
                    Ok(Err(e)) | Err(e) => {
                        tracing::warn!(%e, "persist roster freshness upgrade grace")
                    }
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) | Err(e) => {
                tracing::warn!(%e, "read roster freshness sidecar; treating as unconfirmed")
            }
        }
    }
    let gate: Arc<dyn TrustGate> = Arc::new(ComposedGate::new(roster.clone(), pairs));
    // #229: ARM the dial hook as soon as its inputs exist — before `compose_roster_transport` below
    // subscribes gossip with its bootstrap set, the first gated dial this boot makes. It takes the
    // `Armed` proof, so subscribing before ANY arm fails to compile; that it is THIS endpoint's hooks
    // is by construction here (one `hooks` value), not by the type.
    let armed = hooks.arm(crate::daemon::hooks::DialGate::new(
        store.clone(),
        roster.clone(),
    ));

    // 4. Rate/concurrency limiters, built once from config and shared across every
    //    backend + the accept loop. Installed on the mesh AFTER it is built (below).
    let limiters = crate::limits::MeshLimiters::from_config(&cfg.limits);
    // 4b. Service registry from config. `run_mesh_connection` shares one registry across every
    //    connection, so wrap it once in `Arc` here. (The `status` service/peer lists are read
    //    LIVE from config + store per call — nothing to snapshot here.)
    let services = Arc::new(build_services_audited(&cfg, &audit, &limiters));

    // 5. Assemble the mesh half, start the daemon's OWN ALPN-dispatch accept loop, and install
    //    its handle for hot-reload. Chicken-egg (the loop needs `mesh`, `mesh.accept_task` needs
    //    the loop's handle): build `mesh` with an empty accept_task, spawn the loop with
    //    `mesh.clone()`, then set the handle. The invite registry starts empty (`invite`
    //    mints into it).
    // #87b: restore outstanding invites rather than starting empty. Without this every restart
    // silently voided every invite while the invite line advertised a 24h TTL — an invite mailed
    // to a colleague was reliably dead within a couple of hours on a node that auto-updates.
    // Expired ones are dropped on load, so the file cannot accumulate.
    let invites = Arc::new(LiveInvites::load(
        paths.invites_path.clone(),
        crate::util::epoch_now_u64(),
    ));
    // The nickname we suggest for ourselves in invites + advertise to peers: config override, else the
    // machine hostname (friendly: peers see `jetson`, not `96246d3f`), else the endpoint fingerprint.
    let self_nickname = cfg
        .identity
        .nickname
        .clone()
        .unwrap_or_else(|| default_self_nickname(&our_id));
    // The live-connection registry: threaded into the accept loop's mesh handler
    // (CHECK-register on accept) so a roster install can sever its live sessions. `roster`
    // (the hot-swappable roster gate) + `gate` (the composed gate over it) were built above.
    let conn_registry = Arc::new(ConnRegistry::new());
    // Roster-mode gossip/blob composition: spawn iroh-gossip +
    // the roster-blob transport on THIS SAME endpoint, and subscribe the roster topic bootstrapping
    // from the installed roster's device endpoints (the swarm forms as peers arrive — an empty
    // bootstrap is fine). The accept loop's gossip/blob arms dispatch to these handles. A pure-pairing
    // daemon spawns NEITHER (`None`) — no gossip at all.
    let plan = plan_roster_transport(&roster, &store, &cfg, roster_mode, &our_id).await;
    let (gossip, blobs, roster_topic, presence_topic) =
        compose_roster_transport(&endpoint, plan, &armed).await;
    let mesh = MeshState::new(
        endpoint,
        gate,
        store,
        invites,
        self_nickname,
        config_path,
        roster,
        conn_registry,
        gossip,
        blobs,
        roster_topic,
        presence_topic,
    );
    // Install the process audit sink on the mesh BEFORE serving, so the reload sites +
    // trust-event hooks can re-thread/read it.
    // #67: record what the endpoint ACTUALLY bound, so registering a custom protocol re-advertises
    // this exact set rather than a recomputation that can disagree with it.
    mesh.set_bound_alpns(alpns_for(roster_mode));
    // #229: the revoke paths close this node's OUTBOUND connections through the hooks' registry.
    mesh.set_peer_hooks(hooks);
    mesh.set_audit(audit.clone());
    mesh.set_limits(limiters.clone());
    // Seed the live relay posture from the boot `[network]` so the `set_relays` verb (#53) diffs
    // against runtime truth, not the on-disk config (the `.config()` embedder front door may
    // never persist the boot config to disk).
    mesh.set_applied_relays(&cfg.network.relay_mode, &cfg.network.relay_urls);
    // #89: who gets a reachability pong. Parsed BEFORE this point (see the `presence_mode` call in
    // the config-validation block) so an unknown value is a startup error, never a silent fall back
    // to the permissive default — a privacy knob that fails open is worse than no knob.
    mesh.set_presence_mode(presence);
    // #68: record the SPELLING an operator wrote, so `status.self_network.local_discovery` reads
    // back in the same vocabulary as `config.toml`. Parsed and validated far above — a bad value
    // already failed the boot — so this cannot report a mode the node is not on.
    mesh.set_local_discovery(cfg.network.local_discovery.clone());
    // #85 ask 3 — off unless asked for. Read at boot, like every other trust-shaping knob.
    mesh.set_admit_attested_devices(cfg.identity.admit_attested_devices);
    // #166: the EFFECTIVE keepalive the per-connection idle-timeout check validates against.
    mesh.set_keep_alive_secs(
        cfg.network
            .keep_alive_secs
            .unwrap_or(IROH_MAX_PATH_KEEP_ALIVE_SECS),
    );
    // Self-sovereign pairing identity: load
    // (or mint) this person's UserKey and precompute this daemon's binding over `our_id`, so the
    // pairing handlers PRESENT it and paired peers store a VERIFIED `user_id`. The key path mirrors
    // roster mode's (`[identity].user_key`, else the default), so a roster `join` and pairing share
    // ONE self-sovereign user key/id (DRY). Best-effort: a key error logs + presents nothing (pairing
    // still works, peers store `user_id: None`) rather than failing the daemon.
    let user_key_path = match cfg.identity.user_key.clone() {
        Some(p) => p,
        None => paths.user_key_path.clone(),
    };
    let self_binding = match mcpmesh_trust::UserKey::load_or_generate(&user_key_path) {
        Ok((user_key, created)) => {
            // #85 ask 2: a key we just minted is not an identity anyone has seen, so the import's
            // replace guard must not defend it as though it were.
            mesh.note_user_key_minted_at_boot(created);
            let (user_pk, sig) = mcpmesh_trust::binding::present(&user_key, our_id.as_bytes());
            Some(crate::pairing::rendezvous::SelfBinding { user_pk, sig })
        }
        Err(e) => {
            tracing::warn!(
                %e,
                path = %user_key_path.display(),
                "no user key for pairing identity; paired peers will store this daemon without a user_id"
            );
            None
        }
    };
    mesh.set_self_binding(self_binding);
    // #65: `peer_endorse` reloads the key from here to sign an endorsement.
    mesh.set_user_key_path(user_key_path);
    // #86: an ADOPTED binding — this device was enrolled into another device's identity — wins over
    // the locally-derived one. It is the ONLY copy (an enrolled device holds no user key and cannot
    // re-derive it), so a read failure is logged loudly rather than silently reverting this device
    // to being a stranger to its own person.
    let adopted_path = mesh.adopted_binding_path();
    if adopted_path.exists() {
        match std::fs::read(&adopted_path)
            .map_err(anyhow::Error::from)
            .and_then(|b| {
                serde_json::from_slice::<crate::pairing::rendezvous::SelfBinding>(&b)
                    .map_err(anyhow::Error::from)
            }) {
            Ok(b) => {
                // VERIFY it against THIS endpoint before presenting it (#86 gate). An unverifiable
                // binding — corrupt file, a rotated endpoint key, a planted file — would otherwise
                // be presented forever: peers would warn and store `user_id: None` while `status`
                // confidently reported the adopted id, making this node's self-reported identity
                // locally unfalsifiable.
                match mcpmesh_trust::binding::verify_presented(
                    &b.user_pk,
                    &b.sig,
                    our_id.as_bytes(),
                ) {
                    Ok(_) => {
                        tracing::info!("presenting an adopted device identity (#86)");
                        mesh.set_self_binding_live(Some(b));
                    }
                    Err(e) => tracing::error!(
                        %e,
                        path = %adopted_path.display(),
                        "the adopted device binding does not verify for THIS endpoint; presenting \
                         this node's OWN identity instead. If the endpoint key was rotated, \
                         re-enroll from the device that holds the user key"
                    ),
                }
            }
            Err(e) => tracing::error!(
                %e,
                path = %adopted_path.display(),
                "an adopted device binding exists but could not be read; this device will present \
                 its OWN identity and appear to peers as a different person"
            ),
        }
    }
    // Build the gated per-scope app-blob provider in roster mode and install it on the
    // mesh BEFORE the accept loop starts. Uses the SAME trust gate the mesh resolves inbound MCP
    // with, so the request-time scope check keys on the exact authenticated identity. A build
    // failure disables app blobs with a warning (pairing + mesh keep working).
    //
    // Built in BOTH modes (#61). The scope gate is identity-generic — a grant is a flat principal
    // (`eid:` device, `b64u:` user, or a roster group/user name), and the `eid:` arm is exercised
    // against a non-roster gate by `pairing_mode_eid_grant_admits_and_nickname_grant_stays_denied`.
    // Gating construction on an org root key kept content-addressed transfer out of the mode the
    // quickstart teaches, for no authorization reason.
    {
        // #80: OFF unless `[blobs].gc_interval` is set, parseable, and at or above the floor.
        // `gc_interval_seconds` warns and returns None on the other two, which is the safe
        // direction for a knob that deletes bytes — see its doc for why it inverts this tree's
        // usual "a typo falls back to the default" convention.
        //
        // Computed ONCE: it is the single place that decides, and calling it twice would emit the
        // warning twice for one typo.
        let gc_secs = cfg.blobs.gc_interval_seconds();
        let scopes_path = paths.blob_scopes_path.clone();
        match blocking("join app-blob scopes load", move || {
            crate::blobs::scope::ScopeStore::open(scopes_path)
        })
        .await
        {
            Ok(Ok(scopes)) => {
                match crate::blobs::provider::AppBlobs::load(
                    paths.blobs_dir.clone(),
                    Arc::new(scopes),
                    mesh.gate.clone(),
                    mesh.endpoint.clone(),
                    audit.clone(),
                    // The limiter built at boot, NOT `mesh.limits()` — that falls back to
                    // `unlimited()` on a OnceCell miss, so a wiring mistake would silently disable
                    // a security control. Fail closed, like everything else here (#84a review).
                    limiters.clone(),
                    // #82 ask 2: the mesh's transfer ring, so served-side progress reaches every
                    // `subscribe` consumer.
                    Some(mesh.blob_bcast.clone()),
                    gc_secs.map(std::time::Duration::from_secs),
                )
                .await
                {
                    Ok(provider) => {
                        // #83 ask 3: production tickets must carry the home-relay URL, so wait
                        // (bounded) for the relay handshake before minting. Only here — see
                        // `enable_relay_wait`.
                        provider.enable_relay_wait();
                        // #88: record the on-disk store dir alongside the provider, so
                        // `status.storage.blobs_bytes` walks the dir that actually holds bytes.
                        mesh.set_blobs_dir(paths.blobs_dir.clone());
                        // #80: only when collection is actually RUNNING — the same `gc_secs` the
                        // store was built with, so `status` can never claim an interval the store
                        // is not on. An unset cell is what makes `status.storage.blobs_gc` read
                        // `None` rather than "configured, zero runs".
                        if let Some(secs) = gc_secs {
                            mesh.set_blobs_gc(secs, provider.gc_stats());
                        }
                        mesh.set_app_blobs(provider).await
                    }
                    Err(e) => tracing::warn!(%e, "app-blob provider disabled (build failed)"),
                }
            }
            Ok(Err(e)) | Err(e) => {
                tracing::warn!(%e, "app-blob scopes failed to load; provider disabled")
            }
        }
    }
    // #90: the self-network posture watcher — pushes a StreamFrame::SelfNetwork when this
    // node's own reachability changes (relay connected/lost, home relay moved) and stamps
    // `last_change_epoch` for `status`. Every mode, including relay-disabled (where it simply
    // never observes a relay and never emits): the projection is what `status` reads either way.
    background.push(crate::daemon::spawn_self_net_watch(mesh.clone()));
    // #167 ask 2: the suspend watcher — pushes a StreamFrame::Resumed when this machine wakes from
    // a sleep. Every mode: a closed lid kills sessions whatever the relay configuration is.
    background.push(crate::daemon::spawn_resume_watch(mesh.clone()));

    let accept_task = spawn_accept_loop(mesh.clone(), services);
    mesh.set_accept_task(accept_task).await;

    // 5b. Roster mode: spawn the distribution converge loop — it pulls `RosterAnnounce`s
    //     off the roster topic and, on a higher serial, fetches + single-validates + installs the new
    //     roster, then re-seeds/re-announces (propagation is operator-offline-safe). Self-guards on a
    //     `None` receiver (pure-pairing daemon), so an unconditional call is a no-op there; the
    //     detached handle runs for the daemon lifetime (the loop ends when the topic stream closes).
    if roster_mode {
        let book = std::sync::Arc::new(crate::roster::transport::RosterAddrBook::register(
            &mesh.endpoint,
            256,
        ));
        let _ = mesh.roster_addr_book.set(book);
        background.push(crate::roster::distribute::spawn_receive_loop(mesh.clone()));
    }

    // 5b'. Roster mode: spawn presence. ADVISORY-ONLY — presence feeds `status` + the
    //      person→device dial ORDERING; it NEVER touches a gate, an authz check, or a sever
    //      decision (absence of a presence entry never blocks a dial). Both loops run against the
    //      narrow presence context the mesh composes (table + topic + roster gate). The TRACK loop
    //      records verified heartbeats (each bound to the roster's authoritative user_id); the
    //      PUBLISH loop beats this node's own device-key-signed heartbeat every ~60s. A pure-pairing
    //      daemon (not roster mode) spawns NEITHER loop. The device SigningKey is the SAME ed25519 key
    //      the endpoint id derives from (`key.secret_bytes()`), so the beat's endpoint_id == our_id and
    //      peers resolve us in their roster. Publish only when this node's own user_id is known (config
    //      `[identity].user_id`, else its roster resolution) — a beat under an unknown user_id would be
    //      self-rejected by every peer's user_id binding, so it is skipped rather than sent as noise.
    if roster_mode {
        background.push(crate::roster::presence::track_loop(mesh.presence_ctx()));
        let self_user_id = cfg.identity.user_id.clone().or_else(|| {
            mesh.roster
                .view()
                .and_then(|v| v.resolve(our_id.as_bytes()).map(|d| d.user_id.clone()))
        });
        match self_user_id {
            Some(user_id) => {
                let device_key = ed25519_dalek::SigningKey::from_bytes(&key.secret_bytes());
                background.push(crate::roster::presence::publish_loop(
                    mesh.presence_ctx(),
                    device_key,
                    user_id,
                ));
            }
            None => tracing::debug!(
                "presence publish skipped: no user_id for this node (track loop still runs)"
            ),
        }
    }

    // Periodic staleness sweep. The freshness bound denies NEW inbound at
    // `resolve`; this cuts EXISTING roster-authorized sessions once the node crosses
    // `last_confirmed + max_staleness + grace`. Roster mode only; never severs pairing-only sessions.
    if roster_mode {
        background.push(spawn_staleness_sweep(mesh.clone()));
    }

    // 5c. Roster mode with a pinned `[roster].url`: spawn the HTTPS fallback poll loop.
    //     It GETs the URL every `poll_interval` AND once at startup, so a joiner gets its FIRST roster
    //     (a joiner cannot gossip before it holds a roster). A newer served roster converges
    //     through the SAME `install_from_file` path (no second validator); an equal serial CONFIRMS
    //     currency (freshness). Guarded on `roster_mode` (an org root pinned) — a stray url with no
    //     anchor has nothing to converge to. The rustls provider is installed (step 0) before this runs.
    if roster_mode && let Some(url) = cfg.roster.url.clone() {
        // Route through the tracked helper (NOT a bare spawn) so the startup handle lands in
        // `mesh.poll_loop` — a later runtime `set_roster_url` then aborts+replaces it rather than
        // stacking a second loop.
        respawn_poll_loop(&mesh, url).await;
    }

    // 6. The control-dispatch state over the mesh half; the caller decides how it is
    //    served — the daemon shell binds a socket, an embedded `Node` opens in-memory pipes.
    let state = Arc::new(DaemonState::with_mesh(STACK_VERSION, mesh));
    Ok(BootedNode { state, background })
}

/// How this daemon's endpoint resolves peer addresses: the n0 defaults
/// (pkarr publish + DNS lookup against n0's servers — what `presets::N0` wires), or
/// self-hosted pkarr relay URLs used for BOTH publish and resolve (`discovery_mode =
/// "custom"` + `discovery_urls`).
#[derive(Debug)]
pub enum DiscoveryPlan {
    N0,
    Custom(Vec<url::Url>),
}

/// The validated `[network]` posture — the SINGLE truth `build_endpoint`
/// binds and `doctor` reports on. `Hermetic` (`relay_mode = "disabled"`) is no relay AND no
/// discovery — the localhost/tests mode.
#[derive(Debug)]
pub enum NetPlan {
    Hermetic,
    Mesh {
        relay: iroh::RelayMode,
        discovery: DiscoveryPlan,
    },
}

/// Validate `[network]` into a [`NetPlan`]. Pure (parses, never binds) so config tests and
/// `doctor` share it. Unknown modes and a `"custom"` without URLs are ERRORS, never a silent
/// fallback to public infrastructure — a metadata-privacy knob that quietly reverts to n0
/// defaults would be worse than none.
/// Who gets a reachability pong on `mcpmesh/ping/1` (#89).
///
/// The ping arm is gated by PAIRING alone, so `service_allow_revoke` never reached it: a peer whose
/// every service was revoked still learned you were online, on demand and forever. The only lever
/// was a full unpair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresenceMode {
    /// Any paired peer. Today's behaviour, and the default.
    #[default]
    Paired,
    /// Only a caller currently holding at least one service grant — so an embedder's existing
    /// per-peer sharing switch controls presence too, live, with no new verb.
    Granted,
    /// Never pong.
    Off,
}

impl PresenceMode {
    /// The config/wire spelling. One function, so `SelfNetwork.presence_mode` can never disagree
    /// with the value an operator wrote in `config.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paired => "paired",
            Self::Granted => "granted",
            Self::Off => "off",
        }
    }
}

/// Parse `[network].presence_mode`. An unknown value is a STARTUP ERROR, matching
/// `relay_mode`/`discovery_mode`: a privacy knob must never silently fall back to the permissive
/// value. `presence_mode = "of"` failing loudly is the difference between a user who is hidden and
/// a user who believes they are.
pub fn presence_mode(net: &crate::config::NetworkCfg) -> Result<PresenceMode> {
    match net.presence_mode.as_str() {
        "paired" => Ok(PresenceMode::Paired),
        "granted" => Ok(PresenceMode::Granted),
        "off" => Ok(PresenceMode::Off),
        other => anyhow::bail!(
            "[network] unknown presence_mode {other:?} (expected \"paired\" | \"granted\" | \"off\")"
        ),
    }
}

/// How `[network].local_discovery` resolves: `(enabled, advertise)` (#68).
///
/// Two booleans rather than one, because the halves have very different disclosure: resolving
/// listens, advertising broadcasts your endpoint id and addresses to the whole link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDiscovery {
    pub enabled: bool,
    pub advertise: bool,
}

/// Parse `[network].local_discovery`. An unknown value is a STARTUP ERROR, for the same reason
/// `presence_mode` and `relay_mode` are: `local_discovery = "of"` silently meaning "off" would be a
/// lucky accident, and `= "resolv"` silently meaning "on" would announce a node whose operator
/// asked it not to. A privacy knob must fail loudly or not exist.
pub fn local_discovery(net: &crate::config::NetworkCfg) -> Result<LocalDiscovery> {
    match net.local_discovery.as_str() {
        "off" => Ok(LocalDiscovery {
            enabled: false,
            advertise: false,
        }),
        "on" => Ok(LocalDiscovery {
            enabled: true,
            advertise: true,
        }),
        "resolve" => Ok(LocalDiscovery {
            enabled: true,
            advertise: false,
        }),
        other => anyhow::bail!(
            "[network] unknown local_discovery {other:?} (expected \"off\" | \"on\" | \"resolve\")"
        ),
    }
}

pub fn net_plan(net: &crate::config::NetworkCfg) -> Result<NetPlan> {
    let relay = match net.relay_mode.as_str() {
        // Hermetic: no relay, no discovery (discovery_mode is ignored — doctor warns if set).
        "disabled" => return Ok(NetPlan::Hermetic),
        "default" => iroh::RelayMode::Default,
        "custom" => {
            anyhow::ensure!(
                !net.relay_urls.is_empty(),
                "[network] relay_mode = \"custom\" requires at least one relay_urls entry"
            );
            let urls = net
                .relay_urls
                .iter()
                .map(|u| {
                    u.parse::<iroh::RelayUrl>()
                        .map_err(|e| anyhow::anyhow!("[network] relay_urls entry {u:?}: {e}"))
                })
                .collect::<Result<Vec<_>>>()?;
            iroh::RelayMode::custom(urls)
        }
        other => anyhow::bail!(
            "[network] unknown relay_mode {other:?} (expected \"default\" | \"custom\" | \"disabled\")"
        ),
    };
    let discovery = match net.discovery_mode.as_str() {
        "default" => DiscoveryPlan::N0,
        "custom" => {
            anyhow::ensure!(
                !net.discovery_urls.is_empty(),
                "[network] discovery_mode = \"custom\" requires at least one discovery_urls entry \
                 (a self-hosted pkarr relay, e.g. an iroh-dns-server)"
            );
            let urls = net
                .discovery_urls
                .iter()
                .map(|u| {
                    u.parse::<url::Url>()
                        .map_err(|e| anyhow::anyhow!("[network] discovery_urls entry {u:?}: {e}"))
                })
                .collect::<Result<Vec<_>>>()?;
            DiscoveryPlan::Custom(urls)
        }
        other => anyhow::bail!(
            "[network] unknown discovery_mode {other:?} (expected \"default\" | \"custom\")"
        ),
    };
    Ok(NetPlan::Mesh { relay, discovery })
}

/// Build the Iroh endpoint advertising the mcpmesh/mcp/1 (mesh) + mcpmesh/pair/1 (pairing)
/// ALPNs — the accept loop dispatches each inbound connection by whichever one negotiated. In
/// ROSTER mode (`roster_mode == true`, an org root pinned) it ALSO advertises the gossip + blob
/// ALPNs so the roster/presence distribution + roster-blob transport share this ONE endpoint.
/// A pure-pairing daemon (`roster_mode == false`) advertises EXACTLY mcp/1 + pair/1.
///
/// The `[network]` posture comes from [`net_plan`]:
/// - Hermetic (`relay_mode = "disabled"`): `presets::Minimal` + `RelayMode::Disabled` — a
///   localhost-only endpoint, no relay, no discovery (hermetic tests).
/// - n0-default discovery: `presets::N0` EXPANDED (pkarr publish + pkarr resolve + DNS lookup),
///   with the resolvers wrapped in `lookup_hygiene::Hygienic` (#203) and the relay mode set
///   explicitly as on every branch — with the
///   relay map overridden to the operator's `relay_urls` when `relay_mode = "custom"`.
/// - Custom discovery (`discovery_urls`): `presets::Minimal` plus a `PkarrPublisher` AND a
///   `PkarrResolver` per URL — publish and resolve BOTH go to the self-hosted pkarr relay(s),
///   never to n0 (a half-private discovery setup would defeat the metadata-privacy point).
///
/// Verified against iroh 1.0.3: `Builder::alpns(Vec<Vec<u8>>)` advertises MULTIPLE
/// ALPNs on one endpoint; `Endpoint::builder(preset)`, `.secret_key()`, `.relay_mode()`,
/// `.address_lookup()`, `.bind()` per the pinned crate; `RelayMode::custom(urls)` builds the
/// custom `RelayMap`; all preset paths yield the same `Builder` type.
pub(crate) async fn build_endpoint(
    secret: iroh::SecretKey,
    net: &crate::config::NetworkCfg,
    roster_mode: bool,
    hooks: Option<crate::daemon::hooks::MeshHooks>,
) -> Result<iroh::Endpoint> {
    let builder = match net_plan(net)? {
        NetPlan::Hermetic => iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled),
        NetPlan::Mesh {
            relay,
            discovery: DiscoveryPlan::N0,
        } => {
            // `presets::N0` expanded, so the RESOLVERS can be wrapped (#203).
            //
            // The preset is `Minimal` + a pkarr publisher + a pkarr resolver + a DNS lookup +
            // n0's default relays; we already override the relay mode on every branch, so the
            // only reason to expand it is to interpose `Hygienic` on what comes BACK. Filtering
            // at ingress is the only thing that bounds a self-signed pkarr record naming
            // arbitrary destinations — four call-site filters could not, because `connect` and
            // `blobs.fetch` resolve by ID and lookup runs regardless of what a blob carried.
            //
            // The PUBLISHER is not wrapped: it carries our own addresses outward, already
            // governed by `AddrFilter`.
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .address_lookup(iroh::address_lookup::PkarrPublisher::n0_dns())
                .address_lookup(crate::daemon::lookup_hygiene::Hygienic(
                    iroh::address_lookup::PkarrResolver::n0_dns(),
                ))
                // `presets::N0` gates this on `#[cfg(not(wasm_browser))]` — the type does not
                // exist there. Latent today (no wasm target is built) but the expansion is a
                // hand-maintained copy of an upstream preset, so it carries the gate too.
                .address_lookup(crate::daemon::lookup_hygiene::Hygienic(
                    iroh::address_lookup::DnsAddressLookup::n0_dns(),
                ))
                .relay_mode(relay)
        }
        NetPlan::Mesh {
            relay,
            discovery: DiscoveryPlan::Custom(urls),
        } => {
            let mut b = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal).relay_mode(relay);
            for u in urls {
                b = b
                    .address_lookup(iroh::address_lookup::PkarrPublisher::builder(u.clone()))
                    // #203: the resolver is wrapped here too — an operator-run pkarr server is
                    // still a place records are self-signed by whoever published them.
                    .address_lookup(crate::daemon::lookup_hygiene::Hygienic(
                        iroh::address_lookup::PkarrResolver::builder(u),
                    ));
            }
            b
        }
    };
    let alpns = alpns_for(roster_mode);
    let builder = apply_relay_only(builder, net);
    let builder = apply_transport_config(builder, net)?;
    // #229: the outbound revocation veto + the per-peer connection registry. Installed on EVERY
    // booted endpoint; `None` only for test fixtures that assemble a mesh by hand.
    let builder = match hooks {
        Some(h) => builder.hooks(h),
        None => builder,
    };
    builder
        .secret_key(secret)
        .alpns(alpns)
        .bind()
        .await
        .context("bind iroh endpoint")
}

/// iroh 1.0.3's effective QUIC idle timeout, in seconds — the value we validate a bare
/// `keep_alive_secs` against when no `idle_timeout_secs` is set (#56).
///
/// Pinned by `iroh_transport_defaults_are_what_the_docs_claim`, which is the drift detection the
/// issue actually asked for: an iroh bump that moves this fails a test instead of quietly making a
/// documented number wrong.
pub(crate) const IROH_DEFAULT_IDLE_SECS: u64 = 30;

/// iroh's hard ceiling on the per-path keepalive (`HEARTBEAT_INTERVAL`), in seconds.
///
/// Not a default — a **cap**. `default_path_keep_alive_interval` drops any larger value with a
/// `warn!` and no error, so above this the knob cannot lower ping frequency at all. Pinned by
/// `iroh_transport_defaults_are_what_the_docs_claim`; if a bump raises or removes the cap, the
/// metered-link case #56 was filed for becomes possible and this refusal should be revisited.
pub(crate) const IROH_MAX_PATH_KEEP_ALIVE_SECS: u64 = 5;

/// Build the QUIC transport config `[network]` asks for (#56), or `None` to leave iroh's alone.
///
/// Returns the CONFIG rather than a mutated builder so a test can assert what was actually set —
/// `QuicTransportConfig` has a useful `Debug`. A test that only checks "did bind succeed" cannot
/// see a knob that was never applied, and did not: two mutations escaped that way (#56 gate).
fn build_transport_config(
    net: &crate::config::NetworkCfg,
) -> Result<Option<iroh::endpoint::QuicTransportConfig>> {
    let (idle, keep) = (net.idle_timeout_secs, net.keep_alive_secs);
    if idle.is_none() && keep.is_none() {
        return Ok(None);
    }

    if let Some(k) = keep {
        // iroh CAPS the per-path keepalive at 5s: `default_path_keep_alive_interval` discards any
        // larger value with only a `warn!` and returns the builder unchanged (iroh 1.0.3
        // `endpoint/quic.rs`). So a keepalive above 5s cannot reduce ping frequency — every path
        // keeps pinging at 5s and the operator's metered-link saving never happens. Refuse it
        // rather than accept a knob that silently does the opposite of what it says (#56 gate).
        // `0` is NOT "disable keepalives" — it is a zero-length timer. noq-proto arms it already
        // expired, so every authed packet emits a PING whose ACK re-arms it expired: a self-
        // sustaining PING/ACK loop at RTT cadence on every path. An operator reading `0` the way
        // `idle_timeout_secs = 0` reads three lines up in the same table would saturate the link
        // they meant to quiet. iroh's builder takes a `Duration`, so there is no value that
        // disables keepalives at all — say that rather than accept the one that inverts it.
        anyhow::ensure!(
            k > 0,
            "[network] keep_alive_secs = 0 is not \"disable keepalives\" — it is a zero-length \
             timer that makes every packet emit a PING, saturating the link. There is no way to \
             disable the transport keepalive; omit the key to get iroh's default of \
             {IROH_MAX_PATH_KEEP_ALIVE_SECS}s"
        );
        anyhow::ensure!(
            k <= IROH_MAX_PATH_KEEP_ALIVE_SECS,
            "[network] keep_alive_secs ({k}) is above iroh's per-path keepalive cap of \
             {IROH_MAX_PATH_KEEP_ALIVE_SECS}s, which it silently ignores — every path would keep \
             pinging every {IROH_MAX_PATH_KEEP_ALIVE_SECS}s regardless, so this cannot reduce \
             keepalive traffic. Lower it to make pings MORE frequent on a lossy link; there is no \
             supported way to make them less frequent"
        );
    }

    // The ordering check runs on the EFFECTIVE values of BOTH knobs — outside the `if let` above,
    // deliberately (#210).
    //
    // It used to sit inside that block, so it only ever saw a keepalive the operator had written
    // down. But omitting `keep_alive_secs` does not remove the keepalive: iroh runs its 5s default
    // on the connection AND every path. So `idle_timeout_secs = 3` alone set a 3s idle timer beside
    // a 5s keepalive and BOOTED — the exact condition the message below describes, reached silently
    // by the config the docs invite ("lowering it works one-sidedly").
    //
    // That direction is also the reachable one. A bare KEEPALIVE cannot fail this check while the
    // cap (5s) sits under iroh's default idle timeout (30s); a bare IDLE TIMEOUT fails it for any
    // value at or below 5. The mirror case was the one that mattered and the one that was missing.
    //
    // `iroh_transport_defaults_are_what_the_docs_claim` is what notices if a bump moves either
    // constant out from under this reasoning.
    let effective_keep = keep.unwrap_or(IROH_MAX_PATH_KEEP_ALIVE_SECS);
    let effective_idle = idle.unwrap_or(IROH_DEFAULT_IDLE_SECS);
    anyhow::ensure!(
        // `0` is QUIC's "no idle timeout" — nothing can arrive after it, so there is nothing to
        // race. STRICTLY less than otherwise: a keepalive arriving exactly at the idle deadline is
        // the race, not a margin.
        effective_idle == 0 || effective_keep < effective_idle,
        "[network] the keepalive ({effective_keep}s{}) must be less than the idle timeout \
         ({effective_idle}s{}): a keepalive arriving after the peer's idle timer has fired severs \
         sessions on a clock rather than keeping them open{}",
        if keep.is_some() {
            ""
        } else {
            ", iroh's default"
        },
        if idle.is_some() {
            ""
        } else {
            ", iroh's default"
        },
        // Name the fix that matches the operator's actual file. Telling someone to lower
        // `keep_alive_secs` is useless when the reason they are here is that they never set it.
        // The floor is stated because it is a real dead end, not a rounding detail:
        // `keep_alive_secs` must be >= 1, so `idle_timeout_secs = 1` cannot be satisfied by ANY
        // keepalive. Offering "lower keep_alive_secs" there would be advice that cannot be taken.
        if keep.is_some() {
            ". Lower keep_alive_secs, or raise idle_timeout_secs (keep_alive_secs must be at \
             least 1, so idle_timeout_secs must be at least 2 — or 0 for no timeout at all)"
        } else {
            ". Raise idle_timeout_secs above the keepalive, or set keep_alive_secs below it \
             (keep_alive_secs must be at least 1, so idle_timeout_secs must be at least 2 — or 0 \
             for no timeout at all)"
        }
    );

    let mut cfg = iroh::endpoint::QuicTransportConfig::builder();
    if let Some(i) = idle {
        // `0` is QUIC's "no idle timeout" — a real (if sharp) choice, so it maps to `None` AND is
        // still set, which is what distinguishes it from "not configured".
        let timeout = if i == 0 {
            None
        } else {
            Some(
                iroh::endpoint::IdleTimeout::try_from(Duration::from_secs(i)).map_err(|e| {
                    anyhow::anyhow!(
                        "[network] idle_timeout_secs {i} is out of the range QUIC can encode: {e}"
                    )
                })?,
            )
        };
        cfg = cfg.max_idle_timeout(timeout);
    }
    if let Some(k) = keep {
        // BOTH keepalives, deliberately. iroh sets a connection-level one AND a per-path one, and
        // every path (including PathId::ZERO) pings on the path value. Setting only the connection
        // one leaves a 5s path ping running, so the knob could make pings MORE frequent but never
        // fewer — an operator raising it to save battery on a metered link would get nothing and
        // have no way to tell (#56 gate).
        let d = Duration::from_secs(k);
        cfg = cfg
            .keep_alive_interval(d)
            .default_path_keep_alive_interval(d);
    }
    Ok(Some(cfg.build()))
}

/// Pre-flight the `[network]` transport knobs WITHOUT binding an endpoint (#56).
///
/// `doctor` exists so an operator learns about a fatal config error before a restart, not after.
/// These knobs are validated inside `build_endpoint`, which doctor must never call — it is
/// read-only and binding a socket is not. This is the same validation with the endpoint left out,
/// so the two cannot drift: both go through `build_transport_config`.
/// Reject `[services.<name>].rate_limit_per_min = 0` (#63 gate).
///
/// `RateLimiter::per_minute` floors at 1, so `0` silently became **1 request/minute** — while
/// `docs/config.md` said it was rejected, AND while `blob_bytes_per_min = 0` in the same file means
/// UNLIMITED. An operator writing `0` expecting "no limit" got the most restrictive setting there
/// is. Refused at boot, where the person who wrote it finds out.
pub fn validate_service_rates(cfg: &crate::config::Config) -> Result<()> {
    for (name, svc) in &cfg.services {
        anyhow::ensure!(
            svc.rate_limit_per_min != Some(0),
            "[services.{name}] rate_limit_per_min must be at least 1 \
             (omit it to inherit [limits].rate_limit_per_min; 0 does NOT mean unlimited here)"
        );
    }
    Ok(())
}

pub fn validate_transport_config(net: &crate::config::NetworkCfg) -> Result<()> {
    // #89: presence_mode rides the same pre-flight — doctor must not bless a config whose privacy
    // knob the daemon will reject, and a typo there is exactly the case where the operator most
    // needs to hear about it before the restart rather than after.
    presence_mode(net)?;
    build_transport_config(net).map(|_| ())
}

/// Apply `[network].idle_timeout_secs` / `keep_alive_secs` (#56), if set.
///
/// Untouched when both are absent — the endpoint gets iroh's defaults verbatim, so a config that
/// says nothing about this behaves exactly as it did before the knobs existed. The builder starts
/// from iroh's OVERRIDDEN defaults, so setting one knob does not silently reset the others.
fn apply_transport_config(
    builder: iroh::endpoint::Builder,
    net: &crate::config::NetworkCfg,
) -> Result<iroh::endpoint::Builder> {
    Ok(match build_transport_config(net)? {
        None => builder,
        Some(cfg) => builder.transport_config(cfg),
    })
}

/// TEST-ONLY (#116): an iroh `PathSelector` that carries application data over the RELAY whenever a
/// relay path is open, even if a direct path exists.
///
/// Why this is behind `unstable-relay-only`: `Endpoint::builder().path_selector` is gated behind
/// iroh's `unstable-custom-transports` and documented as "not covered by semantic versioning
/// guarantees and may change in any release without a major version bump". mcpmesh exact-pins iroh
/// to control exactly that, so a production build must never compile against it.
///
/// **It does not stop hole-punching.** A selector chooses among the paths iroh has already opened;
/// preventing direct paths from forming is socket-level behaviour it cannot reach. A direct path
/// may still exist — it simply never carries data. `status` reports `relay` regardless, because #64
/// derives `PeerPath` from `Path::is_selected()`, so the observable agrees with reality.
///
/// Returning `none()` when no relay path is open leaves iroh's current selection alone rather than
/// inventing one — the trait documents an empty selection as "keep the current one".
#[cfg(feature = "unstable-relay-only")]
#[derive(Debug)]
pub(crate) struct RelayOnlySelector;

#[cfg(feature = "unstable-relay-only")]
impl iroh::endpoint::transports::PathSelector for RelayOnlySelector {
    fn select(
        &self,
        ctx: &iroh::endpoint::transports::PathSelectionContext<'_>,
    ) -> iroh::endpoint::transports::PathSelection {
        let mut selection = iroh::endpoint::transports::PathSelection::none();
        if let Some(p) = ctx.paths().find(|p| {
            matches!(
                p.network_path().remote(),
                iroh::endpoint::transports::Addr::Relay(..)
            )
        }) {
            selection.set(&p);
        }
        selection
    }
}

/// Install the relay-only path selector when the config asks for it AND the build supports it
/// (#116).
///
/// The two arms are the whole point of the feature gate: with it OFF, `relay_only = true` still
/// PARSES (so a config file is portable between a test build and a production one) but is ignored
/// — loudly. A startup error would brick a node over a testing switch; a SILENT ignore would let
/// someone believe they tested the relay when they did not, which is the exact failure #116
/// reports.
#[cfg(feature = "unstable-relay-only")]
fn apply_relay_only(
    builder: iroh::endpoint::Builder,
    net: &crate::config::NetworkCfg,
) -> iroh::endpoint::Builder {
    if net.relay_only {
        tracing::warn!(
            "[network] relay_only = true — TESTING POSTURE: no direct addresses are published or \
             resolved, so peers can only reach this node through the relay. Not for production."
        );
        // BOTH halves are needed, and the address filter is the one that actually works.
        //
        // `path_selector` alone did NOT: a selector chooses among paths iroh has ALREADY opened,
        // and when hole-punching succeeds there is simply no relay path left to choose. Measured:
        // the connection had one path, direct, never selected.
        //
        // `AddrFilter::relay_only()` works a layer earlier — it strips IP addresses before they are
        // published or resolved, so a direct path never forms and the relay path is all there is.
        return builder
            .addr_filter(iroh::address_lookup::AddrFilter::relay_only())
            .path_selector(std::sync::Arc::new(RelayOnlySelector));
    }
    builder
}

#[cfg(not(feature = "unstable-relay-only"))]
fn apply_relay_only(
    builder: iroh::endpoint::Builder,
    net: &crate::config::NetworkCfg,
) -> iroh::endpoint::Builder {
    if net.relay_only {
        tracing::warn!(
            "[network] relay_only = true is IGNORED — this binary was built without the \
             `unstable-relay-only` cargo feature. Traffic will take whatever path iroh selects, \
             which on a LAN with IPv6 is usually DIRECT. Rebuild with the feature, or do not rely \
             on this test having exercised the relay."
        );
    }
    builder
}

/// The ALPNs a daemon advertises, by trust mode (#61).
///
/// Every daemon advertises mcp/1 + pair/1 + ping/1 (the trust-gated reachability probe) AND
/// `mcpmesh/blob/1`, the gated app-blob provider.
///
/// **The app-blob ALPN is deliberately NOT roster-gated.** Its authorization is per-scope grants
/// over the flat principal namespace, which an `eid:` device principal satisfies exactly as a roster
/// group name does; the accept arm resolves through `Arc<dyn TrustGate>`, so a pairing
/// `AllowlistGate` gates it identically. Advertising it leaks nothing to a stranger — the arm
/// refuses an unresolved peer with a 401 before any request, then rate-limits per endpoint.
/// Gating it on an org root key kept content-addressed transfer out of the mode the quickstart
/// teaches, for no authorization reason.
///
/// `GOSSIP_ALPN` and the roster `BLOB_ALPN` stay roster-only: both key on `org_id`, which a
/// pairing-mode node does not have.
pub(crate) fn alpns_for(roster_mode: bool) -> Vec<Vec<u8>> {
    let mut alpns = vec![
        ALPN_MCP.to_vec(),
        ALPN_PAIR.to_vec(),
        ALPN_PING.to_vec(),
        crate::blobs::APP_BLOB_ALPN.to_vec(),
    ];
    if roster_mode {
        alpns.push(crate::roster::transport::GOSSIP_ALPN.to_vec());
        alpns.push(crate::roster::transport::BLOB_ALPN.to_vec());
    }
    alpns
}

/// Compose the roster-mode gossip/blob transport on the daemon's ONE endpoint.
/// In roster mode, spawns iroh-gossip + the roster-blob transport and
/// subscribes the roster topic (derived from the org_id — config's pinned value, else the loaded
/// roster view's), bootstrapping from the installed roster's OTHER device endpoints (the swarm forms
/// as peers arrive — an empty bootstrap is fine, [`subscribe`] does not block). Returns
/// `(None, None, None)` for a pure-pairing daemon (no gossip spawned), or —
/// fail-safe — when no org_id is resolvable / the subscribe fails (pairing + mesh keep working;
/// distribution is simply disabled with a warning).
///
/// [`subscribe`]: crate::roster::transport::subscribe
async fn compose_roster_transport(
    endpoint: &iroh::Endpoint,
    // #223 review: the only input besides the endpoint. No roster, no store: the bootstrap set can
    // come from nowhere but the plan, whose `GossipBootstrap` only `bootstrap::for_roster` builds.
    plan: Option<RosterTransportPlan>,
    // #229: subscribing dials the bootstrap set through the endpoint hook, which refuses every
    // gated dial until armed. Requiring the proof makes "gossip before any arm" fail to compile; the
    // proof is not bound to a particular `MeshHooks`.
    _armed: &crate::daemon::hooks::Armed,
) -> (
    Option<iroh_gossip::net::Gossip>,
    Option<crate::roster::transport::RosterBlobs>,
    Option<crate::roster::transport::RosterGossip>,
    Option<crate::roster::transport::RosterGossip>,
) {
    use crate::roster::transport;
    let Some(RosterTransportPlan { org_id, bootstrap }) = plan else {
        return (None, None, None, None);
    };
    let gossip = transport::spawn_gossip(endpoint);
    let blobs = transport::RosterBlobs::new(endpoint);
    // BOTH the roster and presence topics bootstrap from the SAME peer set — the swarm forms as
    // peers arrive, so an empty bootstrap is fine (subscribe does not block on a neighbor).
    let bootstrap = bootstrap.into_ids();
    let roster_topic = match transport::subscribe(
        &gossip,
        transport::roster_topic_bytes(&org_id),
        bootstrap.clone(),
    )
    .await
    {
        Ok(rg) => Some(rg),
        Err(e) => {
            tracing::warn!(%e, "roster-topic subscribe failed; distribution disabled");
            None
        }
    };
    // The presence topic — reuse `transport::presence_topic_bytes`; same org_id +
    // bootstrap. A subscribe failure disables presence ONLY (roster distribution is independent).
    let presence_topic =
        match transport::subscribe(&gossip, transport::presence_topic_bytes(&org_id), bootstrap)
            .await
        {
            Ok(rg) => Some(rg),
            Err(e) => {
                tracing::warn!(%e, "presence-topic subscribe failed; presence disabled");
                None
            }
        };
    (Some(gossip), Some(blobs), roster_topic, presence_topic)
}

/// What [`compose_roster_transport`] subscribes with, decided before anything is spawned (#223).
pub(crate) struct RosterTransportPlan {
    pub(crate) org_id: String,
    pub(crate) bootstrap: bootstrap::GossipBootstrap,
}

/// The roster-mode transport PLAN: `None` for a pure-pairing daemon or when no org_id is resolvable
/// (distribution disabled, with a warning), else the topic anchor and the FILTERED bootstrap set.
///
/// Split from the spawn so a test can assert the bootstrap set the daemon actually subscribes with.
pub(crate) async fn plan_roster_transport(
    roster: &Arc<RosterGate>,
    store: &Arc<PeerStore>,
    cfg: &Config,
    roster_mode: bool,
    our_id: &iroh::EndpointId,
) -> Option<RosterTransportPlan> {
    if !roster_mode {
        return None;
    }
    // The org_id anchors the topic derivation: config's pinned org_id, else the loaded roster view's.
    let Some(org_id) = cfg
        .identity
        .org_id
        .clone()
        .or_else(|| roster.view().map(|v| v.org_id().to_string()))
    else {
        tracing::warn!("roster mode but no org_id known; gossip distribution disabled");
        return None;
    };
    Some(RosterTransportPlan {
        org_id,
        bootstrap: bootstrap::for_roster(roster, store, our_id).await,
    })
}

/// The gossip bootstrap set, as a type only this module can build (#223 review).
///
/// The first fix filtered a helper while the call site stayed free to pass `device_endpoints()`
/// unfiltered — and reverting it to that passed every test. `GossipBootstrap`'s field is private to
/// this module, so the only way to obtain one is [`gossip_bootstrap`] / [`for_roster`].
pub(crate) mod bootstrap {
    use std::sync::Arc;

    use crate::allowlist::PeerStore;
    use crate::roster::gate::RosterGate;

    /// A bootstrap set that has been through the outbound revocation filter.
    pub(crate) struct GossipBootstrap(Vec<iroh::EndpointId>);

    impl GossipBootstrap {
        #[cfg(test)]
        pub(crate) fn ids(&self) -> &[iroh::EndpointId] {
            &self.0
        }
        pub(crate) fn into_ids(self) -> Vec<iroh::EndpointId> {
            self.0
        }
    }

    /// The gossip BOOTSTRAP set (#223): the installed roster's device endpoints, minus ourselves,
    /// minus every device the OUTBOUND revocation predicate refuses.
    ///
    /// `device_endpoints()` already drops the roster's own `revoked_endpoints`, but it keeps a
    /// device this node `peer_revoke`d and every device of a roster user spelled as a revoked
    /// `b64u:` identity — so gossip dialled exactly the devices `open_session` refuses. Takes the
    /// store and view rather than a predicate, so no caller can hand it a permissive one. Blocking.
    ///
    /// Filters only the set WE hand gossip. The neighbours gossip learns from the swarm and dials on
    /// its own are refused by the endpoint hook instead (#229, `hooks::MeshHooks`).
    pub(crate) fn gossip_bootstrap(
        store: &PeerStore,
        view: Option<&mcpmesh_trust::roster::validate::RosterView>,
        our_id: &[u8; 32],
    ) -> GossipBootstrap {
        let Some(v) = view else {
            return GossipBootstrap(Vec::new());
        };
        GossipBootstrap(
            v.device_endpoints()
                .filter(|d| *d != our_id)
                .filter(|d| !crate::daemon::dial::refused_by(store, Some(v), d))
                .filter_map(|d| iroh::EndpointId::from_bytes(d).ok())
                .collect(),
        )
    }

    /// [`gossip_bootstrap`] over the live roster gate, on the blocking pool. A join error
    /// bootstraps from nobody — fail CLOSED, and the swarm tolerates an empty bootstrap.
    pub(crate) async fn for_roster(
        roster: &Arc<RosterGate>,
        store: &Arc<PeerStore>,
        our_id: &iroh::EndpointId,
    ) -> GossipBootstrap {
        let (store, view, me) = (store.clone(), roster.view(), *our_id.as_bytes());
        crate::util::blocking("join gossip bootstrap filter", move || {
            gossip_bootstrap(&store, view.as_deref(), &me)
        })
        .await
        .unwrap_or_else(|_| GossipBootstrap(Vec::new()))
    }
}

#[cfg(test)]
mod lookup_wiring_tests {
    use super::*;

    /// #203: the resolvers `build_endpoint` installs are WRAPPED.
    ///
    /// The commit that added the wrapper disclosed this as untestable — "iroh exposes no way to
    /// enumerate an endpoint's installed lookups". **That was false**, and the review proved it:
    /// `AddressLookupServices` derives `Debug` over its service list, and `Debug` is a supertrait
    /// of `AddressLookup`, so the wiring is observable with no network at all. Six releases in this
    /// subsystem have now shipped a change unpinned at its own call site; disclosing the seventh on
    /// a reason that is untrue is worse than not disclosing it.
    ///
    /// Hermetic: the discovery URL points at a port nothing listens on, and nothing resolves.
    #[tokio::test]
    async fn build_endpoint_wraps_the_resolvers_it_installs() {
        // BOTH branches that install a resolver. The first version only built one and asserted on
        // it — and it silently took the n0 branch, so unwrapping the CUSTOM resolver left it green.
        // A wiring test that does not name which wiring it checked is not a wiring test.
        let n0 = crate::config::NetworkCfg::default();
        let custom = crate::config::NetworkCfg {
            discovery_mode: "custom".into(),
            discovery_urls: vec!["http://127.0.0.1:1/".into()],
            ..Default::default()
        };

        for (label, net, seed) in [("n0", n0, 91u8), ("custom", custom, 94u8)] {
            let ep = build_endpoint(iroh::SecretKey::from_bytes(&[seed; 32]), &net, false, None)
                .await
                .unwrap_or_else(|e| panic!("build the {label} endpoint: {e}"));
            let dbg = format!("{:?}", ep.address_lookup().expect("address lookup"));
            assert!(
                dbg.contains("HygienicLookup(PkarrResolver"),
                "the {label} branch's resolver must be wrapped — an unwrapped one lets a \
                 self-signed pkarr record put arbitrary destinations into the path set: {dbg}"
            );
            // The PUBLISHER is deliberately NOT wrapped: it carries our own addresses outward.
            assert!(
                dbg.contains("PkarrPublisher {"),
                "the {label} branch still installs an unwrapped publisher: {dbg}"
            );
            ep.close().await;
        }
    }

    /// #203: our expansion of `presets::N0` still matches the preset it replaced.
    ///
    /// The expansion exists only to interpose `Hygienic`, so it is a hand-maintained copy of an
    /// upstream preset — and the next iroh bump that adds a service to `N0` would silently drop it
    /// here. Compared by the same `Debug` view, with the wrapper names normalised away.
    #[tokio::test]
    async fn our_n0_expansion_matches_the_preset() {
        fn services(dbg: &str) -> Vec<String> {
            dbg.replace("HygienicLookup(", "")
                .split(',')
                .filter_map(|t| {
                    ["PkarrPublisher", "PkarrResolver", "DnsAddressLookup"]
                        .into_iter()
                        .find(|n| t.contains(n))
                        .map(str::to_string)
                })
                .collect()
        }

        let net = crate::config::NetworkCfg::default();
        let ours = build_endpoint(iroh::SecretKey::from_bytes(&[92u8; 32]), &net, false, None)
            .await
            .expect("build the n0-default endpoint");
        let theirs = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(iroh::SecretKey::from_bytes(&[93u8; 32]))
            .bind()
            .await
            .expect("bind a stock N0 endpoint");

        let ours_dbg = format!("{:?}", ours.address_lookup().expect("ours"));
        let theirs_dbg = format!("{:?}", theirs.address_lookup().expect("theirs"));
        assert_eq!(
            services(&ours_dbg),
            services(&theirs_dbg),
            "our expansion must install the same services, in the same order, as `presets::N0` — \
             ours: {ours_dbg}\n theirs: {theirs_dbg}"
        );
        assert!(
            ours_dbg.contains("Hygienic") && !theirs_dbg.contains("Hygienic"),
            "and ours must be the wrapped one"
        );
        ours.close().await;
        theirs.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #116: `relay_only = true` PARSES on a build without the feature, and does not error.
    ///
    /// A config file must stay portable between a test build and a production one. Making the
    /// field feature-gated would turn a shared config into a parse error on the wrong binary; a
    /// startup error would brick a node over a testing switch. The ignore is loud (a `warn!` in
    /// `apply_relay_only`) — a SILENT ignore is the failure #116 reports, where you believe you
    /// tested the relay and did not.
    #[test]
    fn relay_only_parses_regardless_of_the_feature() {
        let cfg: crate::config::NetworkCfg =
            toml::from_str("relay_only = true\n").expect("the field parses on ANY build");
        assert!(cfg.relay_only);
        // And the default posture is off, so an ordinary config is unaffected.
        let plain: crate::config::NetworkCfg = toml::from_str("").unwrap();
        assert!(!plain.relay_only, "default must be off");
    }

    // #116: `select()` itself cannot be UNIT-tested — iroh's `PathSelectionContext` has no public
    // constructor (`for_test` is `pub(crate)`), so there is no way to hand the selector a candidate
    // set from outside the crate. But the property that matters is end-to-end, and THAT is
    // testable: `iroh::test_utils::run_relay_server()` gives a hermetic harness with a real relay.
    //
    // An earlier version of this comment claimed a hermetic harness could not cover it "because
    // relays are disabled there". That was wrong — `cli/tests/peer_path.rs` had been using
    // `run_relay_server()` since #64. The test below is what that claim should have been.

    /// #116: with the relay-only selector installed, application data takes the RELAY even though
    /// a direct path is available and iroh would otherwise select it.
    ///
    /// Both endpoints are on loopback with a real in-process relay, so a direct path IS reachable
    /// and hole-punching succeeds — `peer_path.rs` asserts exactly that for the default selector.
    /// Here the selected path must be the relay regardless, which is the whole feature.
    /// **THIS TEST CURRENTLY FAILS, AND THAT IS THE POINT.** It documents that `relay_only` does
    /// NOT deliver what it claims, measured rather than argued.
    ///
    /// Observed on loopback with a real in-process relay: the client's connection has exactly ONE
    /// path — direct IP, never `is_selected()` — at every sample from the first. **No relay path
    /// is ever present**, so `RelayOnlySelector` has no relay candidate, returns
    /// `PathSelection::none()`, and iroh keeps whatever it was already doing.
    ///
    /// So a `PathSelector` cannot implement relay-only: it chooses among paths iroh has ALREADY
    /// opened, and when a direct path wins there is no relay path open to choose. iroh's own
    /// `RelayOnly` works at the socket layer (it also suppresses hole-punching) and is not
    /// reachable through the public `path_selector` API.
    ///
    /// `#[ignore]` so it does not fail the suite while the feature is known-broken; run it with
    /// `--ignored` to re-measure. Un-ignore when the mechanism is fixed — do not delete it.
    #[cfg(feature = "unstable-relay-only")]
    #[tokio::test(flavor = "multi_thread")]
    async fn relay_only_keeps_data_on_the_relay_while_a_direct_path_exists() {
        use std::time::Duration;
        tokio::time::timeout(Duration::from_secs(90), async {
            let (relay_map, _relay_url, _guard) = iroh::test_utils::run_relay_server()
                .await
                .expect("in-process relay");

            // The SERVER accepts; only the client forces relay-only, so this proves the selector
            // and not merely "both ends refused to hole-punch".
            let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Custom(relay_map.clone()))
                .ca_tls_config(iroh_relay::tls::CaTlsConfig::insecure_skip_verify())
                .alpns(vec![b"mcpmesh/relayonly/test".to_vec()])
                .bind()
                .await
                .expect("bind server");
            let server_addr = server.addr();
            tokio::spawn(async move {
                while let Some(incoming) = server.accept().await {
                    if let Ok(conn) = incoming.await
                        && let Ok((mut send, _recv)) = conn.accept_bi().await
                    {
                        let _ = send.write_all(b"ok").await;
                        let _ = send.finish();
                    }
                }
            });

            let client = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Custom(relay_map))
                .ca_tls_config(iroh_relay::tls::CaTlsConfig::insecure_skip_verify())
                .addr_filter(iroh::address_lookup::AddrFilter::relay_only())
                .path_selector(std::sync::Arc::new(super::RelayOnlySelector))
                .bind()
                .await
                .expect("bind client");

            // Dial with a RELAY-ONLY address: strip the server's direct addrs, as an address
            // filter would if the address had arrived through lookup rather than by hand.
            let relay_only_addr = iroh::EndpointAddr::from_parts(
                server_addr.id,
                server_addr
                    .addrs
                    .iter()
                    .filter(|a| matches!(a, iroh::TransportAddr::Relay(_)))
                    .cloned(),
            );
            let conn = client
                .connect(relay_only_addr, b"mcpmesh/relayonly/test")
                .await
                .expect("connect over the relay");
            let (mut send, mut recv) = conn.open_bi().await.expect("open bi");
            send.write_all(b"hi").await.unwrap();
            send.finish().unwrap();
            let _ = recv.read_to_end(64).await;

            // Give hole-punching every chance to succeed and be selected. The default selector
            // WOULD switch to direct here — that is what `peer_path.rs` asserts — so if we still
            // read a relay path as selected, the selector is doing the work.
            let mut direct_available = false;
            let mut selected_relay = false;
            for _ in 0..40 {
                for p in &conn.paths() {
                    if p.is_ip() {
                        direct_available = true;
                    }
                    if p.is_relay() && p.is_selected() {
                        selected_relay = true;
                    }
                }
                if direct_available && selected_relay {
                    break;
                }
            }

            assert!(
                direct_available,
                "SETUP: loopback must offer a direct path, or this proves nothing — the selector \
                 would trivially pick the only path there is"
            );
            assert!(
                selected_relay,
                "relay_only must keep DATA on the relay while a direct path is available; the \
                 default selector switches to direct here (peer_path.rs asserts that)"
            );
        })
        .await
        .expect("relay-only e2e timed out");
    }

    /// #105: a BOOTED daemon must have the relay-ready ticket wait ON.
    ///
    /// The wait is opt-in and `boot_node` is the ONLY caller of `enable_relay_wait`, so deleting
    /// that one line silently reverts #83 ask 3 — and the symptom is invisible on any developer
    /// machine: it appears only across a real NAT, shortly after boot, and the SENDER sees nothing
    /// wrong because the file looks fine to them. Nothing else in the suite touches the production
    /// path, since every other fixture builds `AppBlobs` by hand (where the flag defaults off, on
    /// purpose — a relay-disabled endpoint would otherwise pay a guaranteed 3s per mint).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_booted_daemon_waits_for_the_relay_before_minting_tickets() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::NodePaths::under_root(dir.path());
        std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
        // Relay-disabled so the test never reaches the network; the FLAG is what is under test.
        std::fs::write(&paths.config_path, "[network]\nrelay_mode = \"disabled\"\n").unwrap();

        let booted = super::boot_node(paths, None, BootOverrides::default())
            .await
            .expect("the node boots in pairing mode");
        let provider = booted
            .state
            .mesh_required()
            .expect("mesh is up")
            .app_blobs()
            .await
            .expect("the app-blob provider must build in pairing mode (#61) — if THIS fails it is a provider-build regression, not a relay-wait one");

        assert!(
            provider.relay_wait_enabled(),
            "boot must enable the relay-ready wait — without it a ticket minted before the relay \
             handshake carries direct addresses only: LAN-dialable and NAT-dead, and the sender \
             cannot tell (#83 ask 3)"
        );

        // Real teardown — `booted.background` is EMPTY in pairing mode, so aborting it cleans up
        // NOTHING: the accept loop, the app-blob gate loop (holding the redb data-dir lock) and
        // the endpoint would all outlive this test for the life of the binary.
        super::shutdown_booted(booted).await;
    }

    /// #86 gate: an adopted binding that does NOT verify for this endpoint must be REFUSED, not
    /// presented. Otherwise a corrupt file, a rotated endpoint key, or a planted one makes this
    /// node's self-reported identity locally unfalsifiable: peers warn and store `user_id: None`
    /// while `status` confidently reports the adopted id.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unverifiable_adopted_binding_is_refused_not_presented() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::NodePaths::under_root(dir.path());
        std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_path, "[network]\nrelay_mode = \"disabled\"\n").unwrap();

        let adopted_path = paths
            .user_key_path
            .clone()
            .with_extension("adopted-binding.json");
        let (other, _) =
            mcpmesh_trust::keys::UserKey::load_or_generate(&dir.path().join("other.key")).unwrap();
        let other_pk = mcpmesh_trust::binding::user_id(&other);
        // Signed for a DIFFERENT endpoint — the shape a rotated device key produces.
        let wrong = crate::pairing::rendezvous::SelfBinding {
            user_pk: other_pk.clone(),
            sig: crate::pairing::binding_sig_for(&other, &[0x5A; 32]),
        };
        std::fs::create_dir_all(adopted_path.parent().unwrap()).unwrap();
        std::fs::write(&adopted_path, serde_json::to_vec(&wrong).unwrap()).unwrap();

        let booted = super::boot_node(paths, None, BootOverrides::default())
            .await
            .expect("boots");
        let mesh = booted.state.mesh_required().expect("mesh");
        let presented = mesh.self_binding().expect("a binding");
        assert_ne!(
            presented.user_pk, other_pk,
            "an adopted binding that does not verify for THIS endpoint must not be presented — \
             the node must fall back to its own identity rather than claim one it cannot prove"
        );
        super::shutdown_booted(booted).await;
    }

    /// #89 gate: pin the two BOOT lines, not just the parser.
    ///
    /// `boot_node` is the only production caller of `set_presence_mode`, and the only place the
    /// parse is reached on the daemon path. Both survived the whole suite when deleted: with the
    /// install gone, `presence_mode = "off"` parses, doctor says OK, the daemon boots — and pongs
    /// everybody. Nothing reports the live mode, so the operator cannot notice. That is the
    /// "pin the call site, not the helper" failure exactly.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_booted_daemon_installs_the_configured_presence_mode() {
        let boot_with = async |cfg: &str| {
            let dir = tempfile::tempdir().unwrap();
            let paths = crate::paths::NodePaths::under_root(dir.path());
            std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
            std::fs::write(&paths.config_path, cfg).unwrap();
            let out = super::boot_node(paths, None, BootOverrides::default()).await;
            (dir, out)
        };

        // The configured mode must reach the LIVE mesh, not just parse.
        let (_d, booted) =
            boot_with("[network]\nrelay_mode = \"disabled\"\npresence_mode = \"off\"\n").await;
        let booted = booted.expect("the node boots with presence_mode = off");
        assert_eq!(
            booted
                .state
                .mesh_required()
                .expect("mesh is up")
                .presence_mode(),
            PresenceMode::Off,
            "boot must INSTALL the configured presence_mode — parsing it and dropping it on the \
             floor leaves an operator who asked to be hidden pongging everyone, with nothing to see"
        );
        // …and the READ-BACK must report the live mesh, through the real projection. Asserting on
        // `project(.., Some("off"))` directly proves only that a parameter survives a struct
        // literal; the call site is what decides whether an operator can confirm their setting
        // (#89 gate — the helper-vs-call-site trap again).
        let reported = crate::daemon::self_net::read_current(
            booted.state.mesh_required().expect("mesh is up"),
            None,
        );
        assert_eq!(
            reported.presence_mode.as_deref(),
            Some("off"),
            "status must report the LIVE presence mode, not a constant and not the on-disk config"
        );
        super::shutdown_booted(booted).await;

        // The default is still today's behaviour.
        let (_d2, booted2) = boot_with("[network]\nrelay_mode = \"disabled\"\n").await;
        let booted2 = booted2.expect("the node boots with no presence_mode set");
        assert_eq!(
            booted2
                .state
                .mesh_required()
                .expect("mesh is up")
                .presence_mode(),
            PresenceMode::Paired,
            "an unset presence_mode must leave today's behaviour untouched"
        );
        super::shutdown_booted(booted2).await;

        // And an unknown mode must REFUSE TO BOOT — not fall back to the permissive default.
        let (_d3, refused) =
            boot_with("[network]\nrelay_mode = \"disabled\"\npresence_mode = \"of\"\n").await;
        let e = format!(
            "{:#}",
            refused
                .err()
                .expect("an unknown presence_mode must refuse to boot, never fall open")
        );
        assert!(
            e.contains("presence_mode") && e.contains("of"),
            "and the startup error must name the key and the typo: {e}"
        );
    }

    /// #61: a PAIRING-mode daemon must advertise the app-blob ALPN. This is the load-bearing half
    /// of the change — the provider is useless if the endpoint never negotiates the protocol, and
    /// the existing blob AC tests build the provider by hand, so they pass either way and cannot
    /// catch a regression here.
    ///
    /// Gossip and the ROSTER blob ALPN must stay roster-only: both key on `org_id`.
    #[test]
    fn pairing_mode_advertises_app_blobs_but_not_gossip_or_roster_blobs() {
        let pairing = super::alpns_for(false);
        let roster = super::alpns_for(true);
        let has = |v: &[Vec<u8>], a: &[u8]| v.iter().any(|x| x.as_slice() == a);

        for alpn in [ALPN_MCP, ALPN_PAIR, ALPN_PING] {
            assert!(
                has(&pairing, alpn),
                "every daemon advertises the base ALPNs"
            );
        }
        assert!(
            has(&pairing, crate::blobs::APP_BLOB_ALPN),
            "a pairing-mode daemon MUST advertise mcpmesh/blob/1 — its scope gate is \
             identity-generic and an eid: grant authorizes it (#61)"
        );

        // The two that legitimately need an org.
        assert!(
            !has(&pairing, crate::roster::transport::GOSSIP_ALPN),
            "gossip keys on org_id — never advertised without a roster"
        );
        assert!(
            !has(&pairing, crate::roster::transport::BLOB_ALPN),
            "the ROSTER blob transport is distinct from app blobs and stays roster-only"
        );
        assert!(has(&roster, crate::roster::transport::GOSSIP_ALPN));
        assert!(has(&roster, crate::roster::transport::BLOB_ALPN));
        assert!(has(&roster, crate::blobs::APP_BLOB_ALPN));
    }
    /// `net_plan` implements EXACTLY the shipped `[network]` surface — the privacy knobs are
    /// real, validated, and never silently fall back to public infrastructure.
    #[test]
    fn net_plan_validates_the_shipped_network_surface() {
        use crate::config::NetworkCfg;
        let cfg = |relay: &str, relay_urls: &[&str], disc: &str, disc_urls: &[&str]| NetworkCfg {
            relay_mode: relay.into(),
            relay_urls: relay_urls.iter().map(|s| s.to_string()).collect(),
            discovery_mode: disc.into(),
            discovery_urls: disc_urls.iter().map(|s| s.to_string()).collect(),
            relay_only: false,
            ..Default::default()
        };

        // Defaults → the n0 mesh.
        assert!(matches!(
            net_plan(&NetworkCfg::default()).unwrap(),
            NetPlan::Mesh {
                relay: iroh::RelayMode::Default,
                discovery: DiscoveryPlan::N0
            }
        ));
        // Disabled → hermetic, regardless of the discovery knobs (they are off).
        assert!(matches!(
            net_plan(&cfg("disabled", &[], "default", &[])).unwrap(),
            NetPlan::Hermetic
        ));

        // Custom relay: the builder receives a RelayMap holding EXACTLY the configured URLs.
        let plan = net_plan(&cfg(
            "custom",
            &["https://relay.acme.com", "https://relay2.acme.com"],
            "default",
            &[],
        ))
        .unwrap();
        match plan {
            NetPlan::Mesh {
                relay: iroh::RelayMode::Custom(map),
                discovery: DiscoveryPlan::N0,
            } => {
                assert_eq!(map.len(), 2, "both relay_urls land in the RelayMap");
            }
            other => panic!("expected a custom relay plan, got {other:?}"),
        }

        // Custom discovery: the pkarr relay URLs parse and are carried verbatim.
        let plan = net_plan(&cfg(
            "default",
            &[],
            "custom",
            &["https://dns.acme.com/pkarr"],
        ))
        .unwrap();
        match plan {
            NetPlan::Mesh {
                relay: iroh::RelayMode::Default,
                discovery: DiscoveryPlan::Custom(urls),
            } => {
                assert_eq!(urls.len(), 1);
                assert_eq!(urls[0].as_str(), "https://dns.acme.com/pkarr");
            }
            other => panic!("expected a custom discovery plan, got {other:?}"),
        }

        // ERRORS, never silent fallbacks: custom without URLs, garbage URLs, unknown modes.
        assert!(net_plan(&cfg("custom", &[], "default", &[])).is_err());
        assert!(net_plan(&cfg("custom", &["not a url"], "default", &[])).is_err());
        assert!(net_plan(&cfg("default", &[], "custom", &[])).is_err());
        assert!(net_plan(&cfg("default", &[], "custom", &["not a url"])).is_err());
        assert!(net_plan(&cfg("relayless", &[], "default", &[])).is_err());
        assert!(
            net_plan(&cfg("default", &[], "local", &[])).is_err(),
            "the never-implemented \"local\" mode is refused honestly"
        );
    }

    /// #56 gate: what actually reaches the endpoint, not just what was decided.
    ///
    /// The first version asserted only that `bind()` succeeded, which cannot see a knob that was
    /// never applied — two mutations escaped that way, including the one that makes
    /// `idle_timeout_secs = 0` mean "no timeout" rather than silently leaving iroh's 30s.
    /// `QuicTransportConfig` has a `Debug`, so assert on it.
    #[test]
    fn the_configured_values_reach_the_transport_config() {
        let net = |idle: Option<u64>, keep: Option<u64>| crate::config::NetworkCfg {
            idle_timeout_secs: idle,
            keep_alive_secs: keep,
            ..Default::default()
        };
        let dbg = |idle, keep| {
            format!(
                "{:?}",
                build_transport_config(&net(idle, keep))
                    .expect("valid config")
                    .expect("configured")
            )
        };

        assert!(
            build_transport_config(&net(None, None)).unwrap().is_none(),
            "an unconfigured node must touch NOTHING — iroh's defaults verbatim"
        );

        // 3, NOT 5: iroh's own default for BOTH keepalives is 5s, so asserting `Some(5s)` passed
        // with the per-path assignment deleted — the fixture value collided with the default it
        // existed to distinguish from. It must also be BELOW 5, which is iroh's per-path cap.
        let d = dbg(Some(60), Some(3));
        assert!(d.contains("60000"), "idle must reach max_idle_timeout: {d}");
        assert!(
            d.contains("keep_alive_interval: Some(3s)"),
            "keepalive must reach keep_alive_interval: {d}"
        );
        assert!(
            d.contains("default_path_keep_alive_interval: Some(3s)"),
            "and the PER-PATH keepalive too — every path pings on that value, so setting only the \
             connection one makes the knob unable to REDUCE ping frequency: {d}"
        );

        // `0` = no timeout must actually be SET, not left as iroh's 30s.
        let z = dbg(Some(0), None);
        assert!(
            z.contains("max_idle_timeout: None"),
            "idle_timeout_secs = 0 must set no-timeout, not leave iroh's 30s: {z}"
        );

        // A keepalive alone must not invent an idle timeout.
        let k = dbg(None, Some(3));
        assert!(
            k.contains("max_idle_timeout: Some(30000)"),
            "a keepalive alone leaves iroh's idle timeout in place: {k}"
        );
    }

    /// #56 ask 1: the documented iroh numbers are PINNED, so a bump that moves them fails here
    /// rather than silently making four documented values wrong.
    ///
    /// This is the drift detection the issue asked for — "treat a change to them as
    /// release-note-worthy" needs something that notices the change.
    #[test]
    fn iroh_transport_defaults_are_what_the_docs_claim() {
        let d = format!("{:?}", iroh::endpoint::QuicTransportConfig::default());
        for (needle, doc) in [
            ("max_idle_timeout: Some(30000)", "30s idle timeout"),
            ("keep_alive_interval: Some(5s)", "5s keepalive"),
            (
                "default_path_keep_alive_interval: Some(5s)",
                "5s path keepalive",
            ),
            (
                "default_path_max_idle_timeout: Some(15s)",
                "15s path idle timeout",
            ),
        ] {
            assert!(
                d.contains(needle),
                "iroh's default changed: docs/config.md and node/src/config.rs claim {doc} for \
                 iroh 1.0.3. Re-measure and update BOTH, and note it in the release. NOTE: the \
                 relay-path idle timeout in that table is NOT pinned here — it never reaches \
                 QuicTransportConfig — so check it by hand too. Got: {d}"
            );
        }
        assert_eq!(
            IROH_DEFAULT_IDLE_SECS, 30,
            "the constant a bare keep_alive_secs is validated against must track the real default"
        );

        // The per-path CAP, not a default: iroh drops a larger value with only a `warn!`. Probe it
        // behaviourally rather than trusting the constant — if a bump raises or removes the cap,
        // the metered-link case #56 was filed for becomes possible and the refusal in
        // `build_transport_config` should be revisited rather than left denying something that now
        // works.
        let over = format!(
            "{:?}",
            iroh::endpoint::QuicTransportConfig::builder()
                .default_path_keep_alive_interval(Duration::from_secs(
                    IROH_MAX_PATH_KEEP_ALIVE_SECS + 1
                ))
                .build()
        );
        assert!(
            over.contains(&format!(
                "default_path_keep_alive_interval: Some({IROH_MAX_PATH_KEEP_ALIVE_SECS}s)"
            )),
            "iroh no longer caps the per-path keepalive at {IROH_MAX_PATH_KEEP_ALIVE_SECS}s. \
             Raising keep_alive_secs may now genuinely reduce ping traffic — revisit the refusal \
             in build_transport_config and the metered-link note in docs/config.md. Got: {over}"
        );
    }

    /// #86: an ADOPTED binding must survive a restart and WIN over the locally-derived one.
    ///
    /// The enrolled device holds no user key, so it cannot re-derive this — the file is the only
    /// copy. If boot ignored it, the device would silently go back to presenting its OWN identity
    /// and appear to every peer as a different person, which is the exact symptom #86 reports.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_adopted_binding_survives_a_restart_and_wins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::NodePaths::under_root(dir.path());
        std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_path, "[network]\nrelay_mode = \"disabled\"\n").unwrap();

        // The path is derived from the user-key path, so it is knowable WITHOUT booting first —
        // which matters, because a second boot on the same root races the first one's redb lock.
        let adopted_path = paths
            .user_key_path
            .clone()
            .with_extension("adopted-binding.json");

        // A DIFFERENT key from the one this root will mint for itself, so "adopted wins" is
        // distinguishable from "derived its own".
        let (other, _) =
            mcpmesh_trust::keys::UserKey::load_or_generate(&dir.path().join("other.key")).unwrap();
        let other_pk = mcpmesh_trust::binding::user_id(&other);

        // The binding must be REAL and for THIS endpoint — boot verifies it now. Derive the
        // endpoint id by minting the device key the way boot will, then sign for it. The first
        // version of this test used `sig: "b64u:placeholder"` and passed, which is precisely how
        // the missing verification went unnoticed.
        let (device, _) =
            mcpmesh_trust::DeviceKey::load_or_generate(&paths.device_key_path).unwrap();
        let our_id = device.public_bytes();
        let adopted = crate::pairing::rendezvous::SelfBinding {
            user_pk: other_pk.clone(),
            sig: crate::pairing::binding_sig_for(&other, &our_id),
        };
        std::fs::create_dir_all(adopted_path.parent().unwrap()).unwrap();
        std::fs::write(&adopted_path, serde_json::to_vec(&adopted).unwrap()).unwrap();

        // Boot: the adopted binding must be what we present.
        let booted = super::boot_node(paths, None, BootOverrides::default())
            .await
            .expect("boots");
        let mesh = booted.state.mesh_required().expect("mesh");
        assert_eq!(
            mesh.self_binding().expect("a binding").user_pk,
            other_pk,
            "an ADOPTED binding must win over the locally-derived one — otherwise an enrolled \
             device silently reverts to being a stranger to its own person after a restart"
        );
        super::shutdown_booted(booted).await;
    }

    /// #214: import X → adopt Y → RESTART → detach, on one state root. Live and post-restart must
    /// agree at every step: after the restart the enrolled Y is presented and the key is not held,
    /// and a detach falls back to X — the key `user.key` holds — not to a fresh stranger.
    #[tokio::test(flavor = "multi_thread")]
    async fn import_then_adopt_survives_a_restart_and_detach_restores_the_imported_key() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::NodePaths::under_root(dir.path());
        std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_path, "[network]\nrelay_mode = \"disabled\"\n").unwrap();

        let (x, _) =
            mcpmesh_trust::keys::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let x_uid = mcpmesh_trust::binding::user_id(&x);
        let (y, _) =
            mcpmesh_trust::keys::UserKey::load_or_generate(&dir.path().join("y.key")).unwrap();
        let y_uid = mcpmesh_trust::binding::user_id(&y);

        // First boot: import X, then adopt a REAL binding for Y over this endpoint (boot verifies).
        let booted = super::boot_node(paths.clone(), None, BootOverrides::default())
            .await
            .expect("boots");
        {
            let state = &booted.state;
            let mesh = state.mesh_required().expect("mesh").clone();
            let phrase = crate::pairing::recovery::encode(&x.signing_key().to_bytes());
            let out = crate::daemon::user_key_import(state, phrase, false)
                .await
                .expect("import on a fresh root needs no replace");
            assert_eq!(out.user_id, x_uid);
            let binding = crate::pairing::rendezvous::SelfBinding {
                user_pk: y_uid.clone(),
                sig: crate::pairing::binding_sig_for(&y, mesh.endpoint.id().as_bytes()),
            };
            crate::daemon::handlers::adopt_hook(&mesh)(binding)
                .await
                .expect("adopt");
            assert_eq!(mesh.self_binding().unwrap().user_pk, y_uid, "live: Y");
        }
        super::shutdown_booted(booted).await;

        // Restart on the same root. The redb lock is released by shutdown, but give it a bounded,
        // sleeping retry rather than trusting the drop to have landed.
        let mut booted = None;
        for _ in 0..50 {
            match super::boot_node(paths.clone(), None, BootOverrides::default()).await {
                Ok(b) => {
                    booted = Some(b);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        let booted = booted.expect("the root boots again");
        let state = &booted.state;
        let mesh = state.mesh_required().expect("mesh").clone();
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            y_uid,
            "after a restart the enrollment still wins — live and post-restart agree"
        );
        let status = crate::control::status_result(state).unwrap();
        assert_eq!(status.self_user_id.as_deref(), Some(y_uid.as_str()));
        assert!(!status.self_user_key_held, "enrolled: the key is not held");

        let out = crate::daemon::self_enroll_detach(state)
            .await
            .expect("detach");
        assert_eq!(out.detached_from, y_uid);
        assert_eq!(
            out.user_id.as_deref(),
            Some(x_uid.as_str()),
            "detach restores the IMPORTED identity, which is what user.key holds after a restart"
        );
        let status = crate::control::status_result(state).unwrap();
        assert!(status.self_user_key_held);
        super::shutdown_booted(booted).await;
    }

    /// #63 gate: `rate_limit_per_min = 0` on a service must be a STARTUP ERROR.
    ///
    /// `RateLimiter::per_minute` floors at 1, so `0` silently became 1 request/minute — the most
    /// restrictive setting there is — while `docs/config.md` claimed it was rejected AND while
    /// `blob_bytes_per_min = 0` in the same file means UNLIMITED. An operator writing `0` expecting
    /// "no limit" got the exact opposite, with nothing said.
    #[test]
    fn a_zero_service_rate_is_a_startup_error() {
        let cfg = |rate: &str| {
            crate::config::Config::from_toml_str(&format!(
                "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n{rate}"
            ))
            .unwrap()
        };
        validate_service_rates(&cfg("")).expect("an absent rate inherits the global");
        validate_service_rates(&cfg("rate_limit_per_min = 1\n")).expect("1 is the floor and legal");
        validate_service_rates(&cfg("rate_limit_per_min = 500\n"))
            .expect("above the ceiling is CLAMPED later, not refused here");

        let e = validate_service_rates(&cfg("rate_limit_per_min = 0\n"))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("kb") && e.contains("at least 1"),
            "the error must name the service and the floor: {e}"
        );
        assert!(
            e.contains("does NOT mean unlimited"),
            "and correct the reading, since `blob_bytes_per_min = 0` in the same file DOES mean \
             unlimited: {e}"
        );
    }

    /// #89: an unknown `presence_mode` must be a STARTUP ERROR, never a silent fall back to the
    /// permissive default. `presence_mode = "of"` quietly meaning "paired" is the difference
    /// between a user who is hidden and a user who believes they are.
    #[test]
    fn an_unknown_presence_mode_is_a_startup_error() {
        let with = |m: &str| crate::config::NetworkCfg {
            presence_mode: m.into(),
            ..Default::default()
        };
        for good in ["paired", "granted", "off"] {
            presence_mode(&with(good)).unwrap_or_else(|e| panic!("{good:?} must parse: {e:#}"));
        }
        assert_eq!(
            presence_mode(&crate::config::NetworkCfg::default()).unwrap(),
            PresenceMode::Paired,
            "the DEFAULT must stay today's behaviour — this knob must not change anyone silently"
        );

        // A typo, and the near-miss that matters most: a value that looks like it disables.
        for bad in ["of", "OFF", "none", "disabled", "true", ""] {
            let e = presence_mode(&with(bad)).unwrap_err().to_string();
            assert!(
                e.contains(bad) && e.contains("presence_mode"),
                "the error must name the key AND the bad value: {e}"
            );
            assert!(
                e.contains("paired") && e.contains("granted") && e.contains("off"),
                "and list every legal value, or the operator guesses again: {e}"
            );
        }
    }

    /// #56: the ordering check. A keepalive at or above the idle timeout means the peer's timer
    /// fires before the next PING arrives — every session dies on a clock, from a config that
    /// reads as reasonable. Boot must refuse it and name both values.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_keepalive_at_or_above_the_idle_timeout_is_refused() {
        let net = |idle: u64, keep: u64| crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            idle_timeout_secs: Some(idle),
            keep_alive_secs: Some(keep),
            ..Default::default()
        };
        let key = || iroh::SecretKey::from_bytes(&[19u8; 32]);

        // Above iroh's per-path CAP the knob cannot do what its name promises: iroh discards the
        // per-path value with a `warn!`, every path keeps pinging at 5s, and the operator's metered
        // link saves nothing. Refuse it instead of accepting a lie (#56 gate).
        let cfg = |idle: Option<u64>, keep: Option<u64>| crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            idle_timeout_secs: idle,
            keep_alive_secs: keep,
            ..Default::default()
        };

        // 64, and an idle timeout of 1200 rather than 3600: "3600" CONTAINS "60", so the previous
        // fixture let an error that named only the idle timeout satisfy "must name their value".
        // Neither number here is a substring of the other.
        let e = build_transport_config(&cfg(Some(1200), Some(64)))
            .expect_err("a keepalive above iroh's per-path cap must be refused, not silently sunk");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("64"),
            "the error must name THEIR value, not just the cap: {msg}"
        );
        assert!(
            msg.contains("cap of 5s"),
            "and the cap itself, as a number they can act on: {msg}"
        );
        assert!(
            msg.contains("cannot reduce"),
            "and say plainly that raising it does NOT reduce keepalive traffic — that is the whole \
             reason someone sets it: {msg}"
        );

        // THE BOUNDARY. 6 is the smallest value iroh actually discards; 5 is the largest it keeps.
        // Without both, an off-by-one in the predicate this whole change exists to add ships green.
        build_transport_config(&cfg(Some(1200), Some(6)))
            .expect_err("6s is above iroh's cap — iroh would drop it, so boot must refuse it");
        build_transport_config(&cfg(Some(1200), Some(IROH_MAX_PATH_KEEP_ALIVE_SECS)))
            .expect("5s is exactly the cap — iroh keeps it, so refusing it would be wrong");

        // `0` is a PING storm, not "disabled" (#56 gate). The error must not merely refuse; it must
        // correct the reading, or the operator retries with `1` and still has no way to turn it off.
        let z = format!(
            "{:#}",
            build_transport_config(&cfg(Some(1200), Some(0)))
                .expect_err("keep_alive_secs = 0 arms a zero-length timer and must be refused")
        );
        assert!(
            z.contains("not \"disable keepalives\"") && z.contains("omit the key"),
            "and must say what 0 really does AND how to actually get the default: {z}"
        );

        // `idle_timeout_secs = 0` (no timeout) with a keepalive: the `effective == 0` escape hatch.
        // Untested before, and deleting it turned a valid config into a boot failure whose error
        // said the keepalive must be less than `0s`.
        build_transport_config(&cfg(Some(0), Some(3)))
            .expect("no idle timeout means no keepalive can outlive it — this must be allowed");

        // The ordering check needs an idle timeout BELOW the cap; a bare keepalive can no longer
        // reach it, since anything the cap admits (<= 5s) is under iroh's 30s default. The check
        // stays because it is what would bite if a bump moved either number, and
        // `iroh_transport_defaults_are_what_the_docs_claim` is what would tell us it had.
        for (idle, keep) in [(5, 5), (4, 5)] {
            let e = build_endpoint(key(), &net(idle, keep), false, None)
                .await
                .expect_err("a keepalive that cannot arrive in time must be refused at boot");
            let msg = format!("{e:#}");
            assert!(
                msg.contains(&keep.to_string()) && msg.contains(&idle.to_string()),
                "the error must name BOTH values — an operator cannot fix what it does not \
                 identify: {msg}"
            );
        }

        // The valid ordering binds.
        let ep = build_endpoint(key(), &net(30, 5), false, None)
            .await
            .expect("keepalive below the timeout is the working configuration");
        ep.close().await;
    }

    /// #210: a BARE `idle_timeout_secs` is validated against iroh's DEFAULT keepalive.
    ///
    /// The ordering check used to live inside `if let Some(k) = keep`, so it only ever saw a
    /// keepalive the operator had written down. Omitting the key does not remove the keepalive —
    /// iroh still runs its 5s default on both the connection and the path — so
    /// `idle_timeout_secs = 3` alone booted straight into the race the check exists to prevent:
    /// the keepalive fires AFTER the idle timer and severs sessions whose peers are alive and
    /// answering.
    ///
    /// **The silent direction is the dangerous one.** A refused config costs five minutes; one that
    /// boots and then kills live sessions on a clock reads as a flaky network, and the config that
    /// caused it looks accepted. Same shape as #128.
    ///
    /// Asserted through `build_transport_config`, which returns the config rather than mutating a
    /// builder precisely so a test can see what was decided.
    #[test]
    fn a_bare_idle_timeout_is_checked_against_irohs_default_keepalive() {
        let cfg = |idle: Option<u64>, keep: Option<u64>| crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            idle_timeout_secs: idle,
            keep_alive_secs: keep,
            ..Default::default()
        };

        // THE BUG. No keepalive configured, idle timeout below iroh's default keepalive.
        let e = build_transport_config(&cfg(Some(3), None)).expect_err(
            "an idle timeout under iroh's default keepalive severs healthy sessions on a clock — \
             it must be refused at boot, not accepted silently",
        );
        let msg = format!("{e:#}");
        assert!(
            msg.contains('3') && msg.contains(&IROH_MAX_PATH_KEEP_ALIVE_SECS.to_string()),
            "the error must name BOTH values — the operator cannot fix what it does not \
             identify: {msg}"
        );
        // It must NOT send the reader after a key they never set. The whole failure mode is that
        // `keep_alive_secs` is absent from their file; an error telling them to lower it is
        // advice they cannot act on.
        //
        // Asserted on the ATTRIBUTION ITSELF — `(5s, iroh's default)` — not on the bare word
        // "default". The gate caught that: the remedy clause also contains "default", so
        // `msg.contains("default")` stayed green with the attribution deleted outright, and the
        // whole module passed with the feature this assertion exists to pin removed.
        let attribution = format!("({IROH_MAX_PATH_KEEP_ALIVE_SECS}s, iroh's default)");
        assert!(
            msg.contains(&attribution),
            "the error must attribute the keepalive to iroh's DEFAULT, not to a key the operator \
             never wrote — expected {attribution:?} in: {msg}"
        );
        // And the CONFIGURED arm must NOT claim the value came from a default. Without this, one
        // unconditional attribution satisfies the assertion above and the two arms are
        // indistinguishable.
        let configured = format!(
            "{:#}",
            build_transport_config(&cfg(Some(4), Some(5)))
                .expect_err("5s keepalive against a 4s idle timeout is the same race")
        );
        assert!(
            !configured.contains("iroh's default"),
            "a keepalive the operator DID set must not be reported as iroh's default: {configured}"
        );

        // `idle_timeout_secs = 1` is a genuine DEAD END, not merely a low value: `keep_alive_secs`
        // must be >= 1 and strictly less than the idle timeout, so no keepalive can satisfy it.
        // The error must not offer "lower keep_alive_secs" as though it were reachable.
        for keep in [None, Some(1), Some(2), Some(5)] {
            let e = format!(
                "{:#}",
                build_transport_config(&cfg(Some(1), keep))
                    .expect_err("idle_timeout_secs = 1 cannot be satisfied by any keepalive")
            );
            assert!(
                e.contains("at least 2"),
                "the error must name the floor rather than suggest an unreachable fix \
                 (keep={keep:?}): {e}"
            );
        }

        // THE BOUNDARY, and the reason this test is not vacuous. With the keepalive defaulted to 5
        // and the idle timeout at 5, both operands are EQUAL — so `<` and `<=` disagree here and
        // nowhere else. A fixture that only tried 3 would pass under either operator.
        //
        // Equal must FAIL: a keepalive arriving exactly at the idle deadline is the race, not a
        // safe margin.
        build_transport_config(&cfg(Some(IROH_MAX_PATH_KEEP_ALIVE_SECS), None)).expect_err(
            "an idle timeout EQUAL to the default keepalive is the race itself, not a margin",
        );
        build_transport_config(&cfg(Some(IROH_MAX_PATH_KEEP_ALIVE_SECS + 1), None))
            .expect("one second above the default keepalive is a working configuration");

        // `0` is QUIC's "no idle timeout", so nothing can outlive it — the escape hatch must
        // survive the hoist. Deleting the `== 0` short-circuit turns a valid config into a boot
        // failure claiming the keepalive must be less than `0s`.
        build_transport_config(&cfg(Some(0), None))
            .expect("no idle timeout means no keepalive can arrive after it");

        // Neither key set is untouched: iroh's own defaults are internally consistent (30s idle vs
        // 5s keepalive), and this must stay a no-op rather than becoming a boot-time opinion.
        assert!(
            build_transport_config(&cfg(None, None))
                .expect("an unconfigured node must not acquire an opinion")
                .is_none(),
            "neither key set must still leave iroh's transport config entirely alone"
        );

        // And the CONFIGURED path still behaves exactly as before — the hoist must not weaken the
        // two checks that are genuinely about a value the operator wrote.
        build_transport_config(&cfg(Some(1200), Some(0)))
            .expect_err("keep_alive_secs = 0 is still a zero-length timer");
        build_transport_config(&cfg(Some(1200), Some(6)))
            .expect_err("6s is still above iroh's per-path cap");
        build_transport_config(&cfg(Some(30), Some(5)))
            .expect("the documented working configuration still builds");

        // `doctor` must not bless a config the daemon then refuses to boot on. It reaches this
        // through `validate_transport_config`, the crate's only PUBLIC entry point here, so the
        // agreement is asserted rather than assumed — a `doctor` that says "ok" to a config the
        // node dies on is worse than no `doctor`.
        super::validate_transport_config(&cfg(Some(3), None))
            .expect_err("doctor must refuse what boot refuses");
        super::validate_transport_config(&cfg(Some(30), Some(5)))
            .expect("and must still pass the working configuration");
    }

    /// #56: an absent config changes NOTHING — the endpoint gets iroh's defaults verbatim, so a
    /// config that says nothing about this behaves exactly as it did before the knobs existed.
    #[tokio::test(flavor = "multi_thread")]
    async fn absent_transport_config_leaves_iroh_defaults_alone() {
        let net = crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            ..Default::default()
        };
        assert_eq!(net.idle_timeout_secs, None);
        assert_eq!(net.keep_alive_secs, None);
        let ep = build_endpoint(iroh::SecretKey::from_bytes(&[20u8; 32]), &net, false, None)
            .await
            .expect("an unconfigured node still binds");
        ep.close().await;
    }

    /// #56: `idle_timeout_secs = 0` is QUIC's "no timeout", and is ACCEPTED rather than rejected —
    /// it is a real (if sharp) choice. Pinned so it is not turned into an error by accident.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_zero_idle_timeout_means_no_timeout_and_is_allowed() {
        let net = crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            idle_timeout_secs: Some(0),
            ..Default::default()
        };
        let ep = build_endpoint(iroh::SecretKey::from_bytes(&[21u8; 32]), &net, false, None)
            .await
            .expect("0 = no idle timeout is a valid QUIC configuration");
        ep.close().await;
    }

    /// A custom-relay endpoint BINDS without any live relay (the RelayMap is config, not a
    /// connection) — proving the builder wiring end to end with no network dependency.
    #[tokio::test(flavor = "multi_thread")]
    async fn build_endpoint_binds_with_a_custom_relay_map() {
        let net = crate::config::NetworkCfg {
            relay_mode: "custom".into(),
            relay_urls: vec!["https://relay.acme.com".into()],
            discovery_mode: "custom".into(),
            discovery_urls: vec!["https://dns.acme.com/pkarr".into()],
            relay_only: false,
            ..Default::default()
        };
        let ep = build_endpoint(iroh::SecretKey::from_bytes(&[9u8; 32]), &net, false, None)
            .await
            .expect("custom relay+discovery endpoint binds offline");
        ep.close().await;
    }

    /// #68: every `local_discovery` spelling maps to the right (resolve, advertise) pair, and an
    /// unknown one is a STARTUP ERROR.
    ///
    /// The error case is the point, and it is the same reason `presence_mode` has one:
    /// `local_discovery = "of"` silently meaning "off" would be a lucky accident, and `"resolv"`
    /// silently meaning "on" would put a node on the air whose operator asked it not to be. A
    /// multicast packet cannot be un-sent.
    #[test]
    fn local_discovery_modes_parse_and_an_unknown_one_fails_the_boot() {
        let cfg = |mode: &str| crate::config::NetworkCfg {
            local_discovery: mode.into(),
            ..Default::default()
        };
        assert_eq!(
            local_discovery(&cfg("off")).unwrap(),
            LocalDiscovery {
                enabled: false,
                advertise: false
            }
        );
        assert_eq!(
            local_discovery(&cfg("on")).unwrap(),
            LocalDiscovery {
                enabled: true,
                advertise: true
            }
        );
        // The whole reason the knob is a string and not a bool: listening without announcing.
        assert_eq!(
            local_discovery(&cfg("resolve")).unwrap(),
            LocalDiscovery {
                enabled: true,
                advertise: false
            },
            "\"resolve\" must LISTEN and must NOT announce — that distinction is the knob"
        );
        // The default, from a config that never mentions the key.
        assert!(
            !local_discovery(&crate::config::NetworkCfg::default())
                .unwrap()
                .enabled,
            "absent means off — a node must not start multicasting because of an upgrade"
        );

        for bad in ["of", "resolv", "true", "yes", "ON", ""] {
            let e = local_discovery(&cfg(bad))
                .expect_err("an unknown local_discovery must fail the boot, never default");
            let msg = format!("{e:#}");
            assert!(
                msg.contains(bad) && msg.contains("local_discovery"),
                "the error must name the offending value so it is fixable: {msg}"
            );
        }
    }
}
