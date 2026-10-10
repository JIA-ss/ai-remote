//! The long-lived mcpmesh daemon. It owns the single Iroh endpoint (seeded from the device
//! key) and runs two roles simultaneously on one tokio runtime: (1) it runs its
//! OWN accept loop ([`spawn_accept_loop`]) that dispatches each inbound connection by its
//! negotiated ALPN — `mcpmesh/mcp/1` flows through net's gated per-connection handler
//! [`mcpmesh_net::run_mesh_connection`], where `gate` is an
//! [`AllowlistGate`] over the `state.redb` peer allowlist and
//! `services` are backends built from config, while `mcpmesh/pair/1` flows to the pairing
//! rendezvous, GATE-EXEMPT by design and authenticated by the invite secret rather than
//! the trust gate; and (2) it serves the `mcpmesh-local/1` control API on
//! `<runtime_dir>/mcpmesh.sock` (hello + status + service registration + peer add), consumed by
//! the porcelain.
//!
//! The daemon deliberately does not call `mcpmesh_net::serve`: routing by ALPN is the whole
//! point (the gate exemption only applies to the pair ALPN), which the mesh-only `serve`
//! cannot do. `serve`/`ServeHandle` REMAIN in net for its own tests + standalone use; the
//! daemon just composes the same [`mcpmesh_net::run_mesh_connection`] under its own loop.
//!
//! Single-daemon-per-uid: an exclusive non-blocking `flock` on
//! `<runtime_dir>/mcpmesh.lock` is acquired BEFORE any endpoint/store/socket work and held for
//! the process lifetime. This makes `ipc::bind_control_socket`'s stale-socket unlink safe —
//! no LIVE daemon can exist while we hold the lock — and (critically for redb, which takes
//! an exclusive file lock) guarantees exactly one process opens `state.redb`. A redundant
//! daemon loses the lock and exits 0 before touching the device key, store, or endpoint.
mod accept;
pub(crate) mod boot;
pub(crate) mod config_write;
mod conn_cache;
pub(crate) mod dial;
pub(crate) mod dial_hint;
pub(crate) mod handlers;
pub(crate) mod hooks;
pub(crate) mod lookup_hygiene;
mod org_author;
mod path_watch;
pub(crate) mod reach;
mod resume;
mod roster_install;
mod self_net;
mod sever;
mod status;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mcpmesh_net::registry::ConnRegistry;
use mcpmesh_net::{ServiceEntry, Services, SessionBackend, TrustGate};
use mcpmesh_trust::paths;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::allowlist::{AllowlistGate, PeerStore};
use crate::audit::AuditSink;
use crate::backends::socket::SocketBackend;
use crate::backends::spawn::SpawnBackend;
use crate::config::{Backend, Config};
use crate::control::DaemonState;
use crate::pairing::LiveInvites;
use crate::roster::freshness::FreshnessStore;
use crate::roster::gate::RosterGate;
use crate::util::blocking;

use roster_install::roster_confirmed_path;

pub use accept::spawn_accept_loop;
pub use boot::serve_forever;
pub use dial::{dial_service, pipe_session, race_dial};
pub use handlers::{
    BlobWithdrawn, Cancelled, NoSuchBlob, NoSuchBlobScope, NoSuchService, PrincipalRevoked,
    endorse_peer, grant_service_access, grant_service_allow, introduce_peer, peer_revoke,
    peer_unrevoke, remove_peer, rename_peer, revoke_service_access, revoke_service_allow,
};
pub(crate) use handlers::{
    attest_offer, attest_to, device_revocation_import, device_revoke, peer_hint_clear,
};
pub(crate) use org_author::org_rotate;
pub(crate) use reach::caller_admitted_services;
/// The services this identity is admitted to, as the accept path computes them (#100). Test seam:
/// it lets a test assert that the reported set matches what a session would actually be granted.
/// The `status` service list, built from the live registry (#100). Test seam.
#[doc(hidden)]
pub fn service_infos_for_test(
    mesh: &std::sync::Arc<MeshState>,
) -> Vec<mcpmesh_local_api::ServiceInfo> {
    let peers = mesh.store.list().unwrap_or_default();
    service_infos(mesh, &peers)
}

/// Mint an invite (#100 test seam) — pins that `mint_invite` keeps the KNOWN-names view.
#[doc(hidden)]
pub async fn mint_invite_for_test(
    mesh: &std::sync::Arc<MeshState>,
    services: &[String],
) -> anyhow::Result<mcpmesh_local_api::InviteResult> {
    mint_invite(services.to_vec(), None, None, None, false, mesh).await
}

#[doc(hidden)]
pub fn admitted_services_for_test(
    mesh: &std::sync::Arc<MeshState>,
    identity: &mcpmesh_net::PeerIdentity,
) -> Vec<String> {
    caller_admitted_services(mesh, identity)
}
pub use reach::{
    REACH_TTL_SECS, ReachEntry, ReachTransition, normalize_relay_url, probe_peer, reachability_of,
    sanitize_relay_url,
};
pub use resume::{ClockDeltas, ResumeEvent, spawn_resume_watch};
pub(crate) use self_net::read_current as self_network_now;
pub use self_net::spawn_self_net_watch;

/// Live path-watcher tasks (#92 review). `#[doc(hidden)]` — a TEST SEAM for the #61-shaped
/// lifetime regression: a leaked watcher emits nothing, so only a count distinguishes it from a
/// watcher that ended.
#[doc(hidden)]
pub fn live_path_watchers_for_test() -> usize {
    path_watch::LIVE_WATCHERS.load(std::sync::atomic::Ordering::Relaxed)
}
pub use roster_install::{
    install_roster_view_and_sever, should_staleness_sever, staleness_sweep_once,
};

pub use boot::{
    NetPlan, PresenceMode, net_plan, presence_mode, validate_service_rates,
    validate_transport_config,
};
/// The mint-path relay-readiness cap. `pub` so the #125 suite can pin it against a MEASURED
/// `online()` rather than a hardcoded number — the ordering is the contract, not the value.
pub use handlers::RELAY_READY_TIMEOUT;
/// Symmetric with the already-public [`MeshState::register_ephemeral`]: drops in-memory
/// registrations and reloads. Public so tests can exercise what happens to a name held by BOTH an
/// ephemeral registration and `config.toml` once the overlay goes away (#55/#94).
pub use handlers::unregister_ephemeral;
pub(crate) use handlers::{
    add_peer, blob_fetch, blob_fetch_cancel, blob_grant, blob_list, blob_publish, blob_republish,
    blob_revoke, blob_unpublish, mint_invite, open_session, peer_diagnostics, peer_services,
    redeem, register_service, self_enroll_detach, service_allow_grant, service_allow_revoke,
    set_relays, unregister_service, user_key_export, user_key_import,
};
pub(crate) use org_author::{org_approve, org_create, org_join_code, org_revoke};
pub(crate) use roster_install::{
    install_roster, org_join, set_app_metadata, set_nickname, set_roster_url,
};
pub(crate) use status::{
    known_service_names, peer_infos, presence_peers, roster_members, roster_status, service_infos,
};

/// The lockstep stack version (workspace version) reported in `Hello`/`status`.
pub const STACK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default per-service spawn concurrency cap. Each `run` service gets its own
/// semaphore of this size; a socket service has no per-session cap (deliberate). The runtime cap is
/// now config-driven via [`spawn_concurrency`] (`[limits].max_sessions`); this const remains the
/// DOCUMENTED default, pinned to the config default by `spawn_concurrency_reads_max_sessions_*`.
#[allow(dead_code)] // documented default; asserted (test-only) to equal LimitsCfg::default().max_sessions
const SPAWN_CONCURRENCY: usize = 4;

/// The per-service spawn concurrency. Floors to
/// 1 so a `max_sessions = 0` misconfig bounds to one session rather than refusing every session.
pub(crate) fn spawn_concurrency(cfg: &Config) -> usize {
    (cfg.limits.max_sessions.max(1)) as usize
}

/// The relay posture applied to the live endpoint — the runtime "current set" [`MeshState`]
/// tracks for the `set_relays` verb (#53). `mode` is the `[network].relay_mode` the endpoint was
/// built with (or last switched to live); `urls` is the custom relay set (only meaningful when
/// `mode == "custom"`). Default (`mode == ""`) is a pre-seed placeholder overwritten at boot.
#[derive(Clone, Default)]
pub(crate) struct RelayPosture {
    pub(crate) mode: String,
    pub(crate) urls: Vec<String>,
}

/// Why [`MeshState::sign_ceremony_binding`] refused (#221). The ceremony itself only sees `None`
/// and answers the opaque refusal; the reason is for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignBindingRefusal {
    /// This device was ENROLLED into another identity (#86): it holds no key for what it presents.
    Enrolled,
    /// This node presents no identity, so there is nothing to enroll into.
    NoIdentity,
    /// An import or adoption landed since the ceremony snapshotted what it presents.
    IdentityChanged,
    /// No readable user key on disk.
    NoKey,
    /// The key on disk is not the one behind the presented `user_pk`.
    KeyMismatch,
}

/// See [`MeshState::identity_gate_hook`] (#221).
#[cfg(test)]
pub(crate) type IdentityGateHook =
    Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>;

/// The mesh half of the daemon: the endpoint, the trust gate + its backing store, the live
/// invite registry, and the running accept-loop task. Held (behind an `Arc`) inside
/// [`DaemonState`] so the control API's `register_service` / `peer_add` / `pair` methods can
/// hot-reload the registry and populate the store on the SAME open database the gate reads
/// (redb is single-process; routing writes through the daemon is the only correct design).
///
/// It is `Arc<MeshState>` (not owned) because the accept loop and every long-lived roster loop
/// share it. The subsystem modules deliberately never see this struct: the pair rendezvous runs
/// against the narrow [`InviterCtx`] (`inviter_ctx`), the presence loops
/// against [`PresenceCtx`] (`presence_ctx`), and the roster distribution
/// channels against the [`DistributionHost`] seam this struct implements — `MeshState` is the
/// COMPOSER that hands out those contexts, not a parameter the modules take.
///
/// `pub` (fields `pub(crate)`) only so integration tests can assemble one via [`MeshState::new`]
/// + [`MeshState::set_accept_task`] and drive the SAME accept loop the daemon runs.
///
/// [`InviterCtx`]: crate::pairing::rendezvous::InviterCtx
/// [`PresenceCtx`]: crate::roster::presence::PresenceCtx
/// [`DistributionHost`]: crate::roster::distribute::DistributionHost
pub struct MeshState {
    pub(crate) endpoint: iroh::Endpoint,
    pub(crate) gate: Arc<dyn TrustGate>,
    pub(crate) store: Arc<PeerStore>,
    /// The in-RAM registry of outstanding pairing invites. The accept loop's
    /// `mcpmesh/pair/1` branch redeems against it; shared with every spawned pair handler.
    pub(crate) invites: Arc<LiveInvites>,
    /// This device's suggested name for itself, carried in a minted invite.
    /// Resolved at startup (config `identity.nickname`, else a short base32 fingerprint of
    /// the endpoint id) and LIVE-updatable by the `set_nickname` verb (#37) — hence the
    /// `RwLock` (std, never held across await; read-clone via [`self_nickname`](Self::self_nickname)).
    /// The redeemer stores it as its local name for us.
    pub(crate) self_nickname: std::sync::RwLock<String>,
    /// The daemon's own ALPN-dispatch accept loop (see [`spawn_accept_loop`]). Started once and
    /// held for the process lifetime: since #54 a hot-reload swaps [`services`](Self::services)
    /// in place rather than aborting and respawning this loop, so there is no serving blip and a
    /// reload reaches connections that are already open.
    pub(crate) accept_task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    /// The running HTTPS roster-poll loop, if `[roster].url` is set. `None` until a URL is
    /// pinned. Held so [`respawn_poll_loop`] can ABORT+REPLACE it — a runtime `set_roster_url`
    /// (a joiner's first-roster bootstrap, or a URL change) (re)starts polling WITHOUT a daemon
    /// restart, and repeated calls never STACK duplicate loops (the idempotency guard). Initialized
    /// `None` inside [`new`](Self::new) (like `accept_task`), so no call site changes.
    ///
    /// [`respawn_poll_loop`]: roster_install::respawn_poll_loop
    pub(crate) poll_loop: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    /// Serializes the WHOLE config-mutating critical section — `register_service` (config read →
    /// upsert → atomic write → reload → rebuild → accept-loop swap → status refresh) AND the
    /// pairing `grant_service_access` (allow-append → reload → swap). Without it, a concurrent
    /// registration and a pairing-grant each read the same base config and the second write
    /// clobbers the first's change (lost update). The redb `peer_add` path is already serialized
    /// by redb's write lock; this gives the config path an equivalent.
    pub(crate) reload_lock: tokio::sync::Mutex<()>,
    /// `[network].presence_mode` (#89), resolved at boot. Who gets a reachability pong.
    ///
    /// Not live-editable: changing the MODE needs a restart. The per-peer effect under
    /// `Granted` is live anyway, because service grants are — revoking the last service takes
    /// presence with it in the same action, which is the property an embedder's per-peer sharing
    /// switch needs.
    pub(crate) presence_mode: std::sync::RwLock<crate::daemon::PresenceMode>,
    /// `[network].local_discovery` (#68), as spelled in config, resolved at boot.
    ///
    /// Stored as the SPELLING rather than the parsed pair, because its only consumer is
    /// `status.self_network` reading it back to the operator — and a read-back in a different
    /// vocabulary from the one they wrote cannot confirm their setting took.
    pub(crate) local_discovery: std::sync::OnceLock<String>,
    /// `[identity].admit_attested_devices` (#85 ask 3), resolved at boot.
    pub(crate) admit_attested: std::sync::OnceLock<bool>,
    /// The effective `[network].keep_alive_secs` (#166), resolved at boot.
    pub(crate) keep_alive_secs: std::sync::OnceLock<u64>,
    /// #229: the endpoint hooks `build_endpoint` installed — the dial gate and the registry of this
    /// node's live connections by remote id. Set once at boot. UNSET on a mesh a test assembles by
    /// hand over an unhooked endpoint, where there is nothing registered to close.
    pub(crate) peer_hooks: std::sync::OnceLock<hooks::MeshHooks>,
    pub(crate) config_path: PathBuf,
    /// The relay posture (mode + custom URL set) currently APPLIED to the live endpoint — the
    /// runtime truth the `set_relays` verb (#53) diffs against. Seeded at boot from `[network]`
    /// via [`set_applied_relays`](Self::set_applied_relays) and updated on each successful LIVE
    /// `set_relays`. In-memory on purpose: the `.config()` embedder front door may never persist
    /// the boot config to disk (see `NodeBuilder::config`), so the config FILE is not a reliable
    /// "current set" — this is. `Mutex` because `set_relays` mutates it under the reload lock.
    pub(crate) applied_relays: std::sync::Mutex<RelayPosture>,
    /// The roster-mode gate handle (hot-swapped on install; consulted for the sever set + status).
    /// `RosterGate::empty()` in a pure-pairing daemon — where [`ComposedGate`] then falls through to
    /// pairing for everything, exactly as a pairing-only build behaved. In a roster daemon this is the SAME
    /// `Arc<RosterGate>` [`ComposedGate`] holds, so [`install_roster_view_and_sever`] hot-swaps the
    /// live gate's view with a single `install` (no gate rebuild).
    ///
    /// [`ComposedGate`]: crate::roster::gate::ComposedGate
    pub(crate) roster: Arc<RosterGate>,
    /// Live mesh connections, for revocation-severing on roster install.
    /// [`spawn_accept_loop`] threads it into [`run_mesh_connection`] (CHECK-register on accept); the
    /// install path calls [`ConnRegistry::sever_matching`] against it.
    pub(crate) conn_registry: Arc<ConnRegistry>,
    /// The LIVE service registry every accepted connection resolves its sessions against.
    /// Installed by [`spawn_accept_loop`] and hot-swapped by
    /// [`swap_services`](crate::daemon::accept::swap_services) on every reload (grant, revoke,
    /// register, roster install) — so a reload reaches connections that are ALREADY open, which
    /// the old abort-and-respawn of the accept loop never did (#54). Swapping in place also
    /// removes the window in which the accept loop was down.
    pub(crate) services: Arc<mcpmesh_net::LiveServices>,
    /// Test-only hook fired at the START of a sever, with the LIVE registry as of that instant
    /// (#99).
    ///
    /// #54's SWAP-BEFORE-SEVER ordering — install the new registry, THEN cut live connections —
    /// is a security property: swap first and no NEW session is admitted under the pre-revoke
    /// allow while the in-flight ones are being cut. Reversing the two statements is invisible
    /// from outside the verb, because by the time it returns both have happened. This seam is the
    /// only way to observe the order without racing the wire.
    ///
    /// `None` in every production path; nothing installs it but tests.
    #[allow(clippy::type_complexity)]
    pub(crate) sever_observer:
        std::sync::Mutex<Option<Arc<dyn Fn(&mcpmesh_net::Services) + Send + Sync>>>,
    /// The roster/presence gossip handle + roster-blob transport, spawned on the
    /// daemon's ONE endpoint (#54). `None` in a pure-pairing daemon (no org root
    /// pinned) — no gossip is spawned, exactly the pairing-only behavior. [`spawn_accept_loop`]'s gossip/blob
    /// arms dispatch inbound connections to these; a `None` arm closes the connection cleanly.
    pub(crate) gossip: Option<iroh_gossip::net::Gossip>,
    pub(crate) blobs: Option<crate::roster::transport::RosterBlobs>,
    /// The roster-topic subscription: the sender announces (cloned per call), the receiver is
    /// moved out ONCE by the converge loop. `None`/empty in a pure-pairing daemon.
    pub(crate) roster_topic: tokio::sync::Mutex<Option<crate::roster::transport::RosterGossip>>,
    /// The presence-topic subscription: the publish loop clones the sender to broadcast
    /// heartbeats; the track loop moves the receiver out ONCE. `None`/empty in a pure-pairing
    /// daemon. Behind an `Arc` because `presence_ctx` shares the SAME
    /// handle with the presence loops (which own the sender/receiver access).
    pub(crate) presence_topic:
        Arc<tokio::sync::Mutex<Option<crate::roster::transport::RosterGossip>>>,
    /// The advisory presence table: the track loop records verified heartbeats here;
    /// `status` + the person→device dial read it for recency ORDERING. ADVISORY-ONLY — no gate,
    /// authz check, or sever decision ever consults it (absence never blocks a dial). Always present
    /// (constructed in [`new`](Self::new)); a pure-pairing daemon simply never records into it.
    pub(crate) presence_table: Arc<crate::roster::presence::PresenceTable>,
    /// OPTIONAL embedder-set app metadata (#39): an opaque ≤256B blob folded (signed) into
    /// this node's outgoing presence heartbeats. `Arc<RwLock<..>>` so the detached publish
    /// loop reads it FRESH each beat via [`presence_ctx`](Self::presence_ctx). Empty = unset.
    /// In-memory only (lost on restart; the embedder re-sets on startup).
    pub(crate) app_metadata: Arc<std::sync::RwLock<String>>,
    /// The gated per-scope app-blob provider. Present in BOTH trust modes since #61 — grants are
    /// flat principals (`eid:` device / `b64u:` user / roster group or user name), so the scope gate
    /// never needed a roster. `None` only until [`set_app_blobs`](Self::set_app_blobs) installs it,
    /// or if the store failed to build. Behind a `tokio::sync::Mutex<Option<..>>` set post-construction (like
    /// `accept_task`/`poll_loop`), so `MeshState::new`'s signature is unchanged and no existing caller
    /// breaks. The accept loop's `APP_BLOB_ALPN` arm reads it per-connection.
    pub(crate) app_blobs: tokio::sync::Mutex<Option<Arc<crate::blobs::provider::AppBlobs>>>,
    /// The process-wide audit sink. Set ONCE by [`serve_forever`] before serving via
    /// [`set_audit`](Self::set_audit); read by the reload sites (to re-thread it into rebuilt
    /// backends) and the trust-event hooks. `OnceLock` — set-once, lock-free reads, no async.
    /// Empty (→ `AuditSink::disabled()`) in a control-only test daemon.
    pub(crate) audit: std::sync::OnceLock<AuditSink>,
    /// The on-disk app-blob store directory (#88), set once at boot when one exists — read by
    /// `status.storage.blobs_bytes`. Same set-once discipline as `audit`/`limits`.
    pub(crate) blobs_dir: std::sync::OnceLock<PathBuf>,
    /// The configured blob-GC interval and its counters (#80), set once at boot alongside
    /// `blobs_dir` — and ONLY when collection is actually configured, so `status` can tell "not
    /// collecting" from "collecting, has not run yet". Read synchronously by
    /// `status.storage.blobs_gc`; the `app_blobs` mutex could not be taken from there.
    pub(crate) blobs_gc: std::sync::OnceLock<(u64, Arc<crate::blobs::provider::BlobGcStats>)>,
    /// Self-network transition ring (#90) — `subscribe`'s third tap, fed by
    /// [`spawn_self_net_watch`](self_net::spawn_self_net_watch). A SEPARATE ring from
    /// `reach_bcast` for the same reason that one is separate from audit: the frames have
    /// different shapes and different producers, and merging happens at the subscription.
    pub(crate) self_net_bcast: tokio::sync::broadcast::Sender<mcpmesh_local_api::SelfNetwork>,
    /// Suspend/resume ring (#167 ask 2) — `subscribe`'s fifth tap, fed by
    /// [`spawn_resume_watch`](resume::spawn_resume_watch). Its own ring for the same reason as the
    /// others: a different shape, a different producer, merged at the subscription.
    pub(crate) resume_bcast: tokio::sync::broadcast::Sender<resume::ResumeEvent>,
    /// The duplicate-identity observation (#134) — the SAME cell as whatever
    /// [`IdentityConflictLayer`](crate::diag::IdentityConflictLayer) is recording into. Set once
    /// by the standalone daemon at boot, or by an embedder via
    /// [`NodeBuilder::identity_conflict`](crate::NodeBuilder::identity_conflict).
    ///
    /// Set-once (same discipline as `audit`/`limits`) rather than owned, because the layer must be
    /// constructed with this Arc BEFORE the host installs its subscriber, which happens before any
    /// node exists. Unset — the default — means the condition is not observable here, which is why
    /// `status` reports absence rather than "no conflict".
    pub(crate) identity_conflict: std::sync::OnceLock<Arc<crate::diag::IdentityConflict>>,
    /// When the self-net watcher last observed a posture change (#90, epoch seconds) — merged
    /// into `status.self_network.last_change_epoch`. A std Mutex, never held across an await.
    pub(crate) self_net_change: std::sync::Mutex<Option<i64>>,
    /// The process rate/concurrency limiters. Set ONCE by `serve_forever` before
    /// serving (like `audit`); read by the reload sites (rebuilt backends re-thread it) and the
    /// accept loop. Empty (→ an unlimited default) in a control-only test daemon.
    pub(crate) limits: std::sync::OnceLock<Arc<crate::limits::MeshLimiters>>,
    /// The bounded provider address book for roster-blob fetches. Registered once in
    /// roster mode; a per-announce address add goes through it (bounded). `None` (unset) → tests use
    /// the per-fetch fallback. Kept in a OnceLock (like `audit`/`limits`) so `MeshState::new` is
    /// unchanged.
    pub(crate) roster_addr_book:
        std::sync::OnceLock<std::sync::Arc<crate::roster::transport::RosterAddrBook>>,
    /// This daemon's precomputed self-sovereign identity presentation for pairing (the device->user
    /// binding, identity). Loaded ONCE by [`serve_forever`] from the config
    /// `[identity].user_key` (auto-generated if absent) and signed over THIS endpoint via
    /// [`set_self_binding`](Self::set_self_binding) — same set-once discipline as `audit`/`limits`.
    /// `None` (unset) in a control-only/test daemon or when no user key exists → the pairing handlers
    /// present nothing and paired peers store `user_id: None` (the pre-identity behavior).
    pub(crate) self_binding: std::sync::OnceLock<Option<crate::pairing::rendezvous::SelfBinding>>,
    /// Where this person's `UserKey` lives (#65), resolved once at boot from `[identity].user_key`
    /// (else the default). `peer_endorse` reloads it to SIGN an endorsement — the key is never held
    /// in memory beyond a request, matching how boot uses it.
    pub(crate) user_key_path: std::sync::OnceLock<PathBuf>,
    /// Did BOOT mint the user key, rather than load an existing one (#85 ask 2)?
    ///
    /// The import's `replace` guard exists to stop someone discarding a real identity. Without this
    /// it could not tell one from a key minted 200 ms earlier by the very daemon the import
    /// auto-started — so on the primary use case, a NEW laptop, `identity import` always refused
    /// and pushed the user to a flag whose help text says it destroys things irreversibly. Training
    /// people to pass that flag is worse than not having it.
    ///
    /// A key this node minted itself and has never presented to anyone is not an identity worth
    /// protecting; a key it loaded from disk might be.
    ///
    /// **"Is the key ON DISK still the one boot minted"**, not "did boot mint one" (#221). It was a
    /// set-once `OnceLock` no import cleared, so a second import in the same daemon lifetime found
    /// it still `true` and silently discarded the first imported key under `replace: false`. An
    /// import clears it ([`note_user_key_replaced`](Self::note_user_key_replaced)); nothing sets it
    /// back but boot. Set by boot before serving; read and cleared under `user_key_lock`. Unset
    /// (`false`) fails CLOSED: the guard applies.
    user_key_still_boot_minted: std::sync::atomic::AtomicBool,
    /// A user key RESTORED from a recovery phrase (#85 ask 2), overriding the boot-derived binding.
    ///
    /// **Separate from [`adopted_binding`](Self::adopted_binding), and the distinction is
    /// load-bearing.** That field does not mean "the binding to present" — it means *this device
    /// was enrolled into someone else's identity and holds no authority over it*, and the #86 gate
    /// (`handlers::lock_own_user_key`) reads exactly that: `peer_endorse`, `device_revoke` and
    /// `user_key_export` refuse, and `sign_binding` returns `None` so `invite --as-self` cannot
    /// enroll a third device.
    ///
    /// An IMPORT is the opposite situation: the device now holds that user key. Reusing the
    /// adopted slot for it made a freshly-recovered machine unable to endorse or to enroll its
    /// owner's other devices — the very remedy the recovery CLI prints — with an error message
    /// stating it did not hold a key it had just imported. Caught by review, by probe.
    pub(crate) imported_binding: std::sync::RwLock<Option<crate::pairing::rendezvous::SelfBinding>>,
    /// A self-enrollment binding ADOPTED from another device of this person (#86), overriding the
    /// locally-derived one. `RwLock`, not `OnceLock`: enrolling installs it live, and a device may
    /// later be re-enrolled into a different identity.
    ///
    /// **`is_some()` means "this device holds NO user key of its own"** — see
    /// [`imported_binding`](Self::imported_binding) for why that is not the same question as which
    /// binding to present, and for what happened when the two were conflated.
    pub(crate) adopted_binding: std::sync::RwLock<Option<crate::pairing::rendezvous::SelfBinding>>,
    /// Recent INVITER-side pairing completions — a tiny in-memory ring (cap
    /// [`RECENT_PAIRINGS_CAP`]) `status` surfaces so the inviter's HUMAN can read the SAS and
    /// compare it with the redeemer's out-of-band ("both humans compare the code"; the
    /// redeemer gets the code in its `PairResult`, this is the inviter's porcelain path to the
    /// same words). DISPLAY-ONLY ceremony state, NOT trust data: never persisted, lost on daemon
    /// restart (acceptable — the ceremony happens right after the pair), never an authorization
    /// input. std `Mutex` (never held across an await; push/snapshot are sync + tiny).
    pub(crate) recent_pairings:
        std::sync::Mutex<std::collections::VecDeque<mcpmesh_local_api::RecentPairing>>,
    /// On-demand reachability probe cache (pairing-mode liveness). Keyed by endpoint-id INTERNALLY;
    /// [`probe_peer`] writes it and [`reachability_of`] reads it (projecting to the NICKNAME —
    /// never the id). In-memory + ephemeral: never persisted, lost on restart, never an
    /// authorization input (advisory presence only). std `Mutex` — held only for the tiny
    /// insert/clone, never across an await.
    pub(crate) reachability: std::sync::Mutex<std::collections::HashMap<[u8; 32], ReachEntry>>,
    /// Live fan-out of reachability TRANSITIONS to `subscribe`rs (#58) — the pushed liveness
    /// signal that replaces polling `status`.
    ///
    /// A SEPARATE ring from the audit broadcast on purpose: the audit sender is the same call that
    /// appends to the on-disk log, so routing probe results through it would either write them into
    /// the audit file or split record-from-broadcast. Keeping them apart leaves the audit log's
    /// schema exactly as it is. Sends are best-effort — no subscribers is the common case and a
    /// `send` error there is expected, never an error.
    ///
    /// Carries the PRODUCER alongside the row (#150): the two senders — [`reach::probe_peer`] and
    /// [`path_watch::commit_observation`] — are the only places that know which one ran, so the
    /// attribution is stamped at the `send` rather than guessed at the subscription.
    pub(crate) reach_bcast: tokio::sync::broadcast::Sender<ReachTransition>,
    /// App-blob transfer progress (#82 ask 2). Its OWN ring, for the same reason `reach_bcast` is
    /// separate from audit: a transfer emits many frames over its life and must not evict audit
    /// records, which are the compliance surface.
    ///
    /// The producer COALESCES — iroh-blobs reports per ~16 KiB chunk, so an uncoalesced 4 GiB
    /// transfer would push ~262k frames and every subscriber would see `Lagged`.
    pub(crate) blob_bcast: tokio::sync::broadcast::Sender<BlobTransfer>,
    /// Monotonic probe ticket source (#58 review). Probes of one peer overlap and complete out of
    /// order; each takes a ticket at START so a slow earlier probe cannot overwrite a fast later
    /// one — see [`ReachEntry::seq`].
    pub(crate) probe_seq: std::sync::atomic::AtomicU64,
    /// Peers with a background refresh already in flight (#176), so [`reach::reachability_of`]
    /// spawns ONE probe per peer instead of one per poll.
    ///
    /// This replaces the "known, bounded v1 tradeoff" that used to live at the spawn site. It was
    /// bounded in probe COUNT and not in damage: a caller polling `status` faster than
    /// `PROBE_TIMEOUT` spawned a fresh dial every poll, and the resulting contention is what made
    /// the last-started probe — the one whose verdict used to win — the one most likely to time
    /// out. Entries are released by a drop guard, so a probe that panics or is cancelled cannot
    /// wedge a peer into never being refreshed again.
    ///
    /// std `Mutex`, held only for the insert/remove and never across an await.
    pub(crate) probes_inflight: std::sync::Mutex<std::collections::HashSet<[u8; 32]>>,
    /// The per-peer client connection cache for MCP sessions (#215) — see `conn_cache`.
    pub(crate) conn_cache: conn_cache::McpConnCache,
    /// Serializes an org AUTHORING verb's whole read-modify-write (#66).
    ///
    /// `org_approve`/`org_revoke` read `roster.json`, mutate, bump the serial, re-sign, and only
    /// then install — and `install_roster` takes `reload_lock` for the install alone. Since #172 the
    /// control API dispatches a connection's requests CONCURRENTLY, so two approvals can both read
    /// serial N and both build N+1. There is no lost update (the second is refused by the
    /// `serial > installed` rollback check), but the loser fails with a roster-validation sentence
    /// about serial monotonicity, which is an opaque and misleading answer for an "approve this
    /// person" button. Holding this across the whole RMW makes the second approval simply queue and
    /// then succeed at N+2.
    ///
    /// A SEPARATE lock rather than widening `reload_lock`: `install_roster` takes that one itself,
    /// and tokio's `Mutex` is not reentrant, so reusing it here would deadlock. Ordering is
    /// authoring → `reload_lock` and never the reverse — nothing acquires this while holding
    /// `reload_lock` — so the graph stays acyclic.
    pub(crate) org_author_lock: tokio::sync::Mutex<()>,
    /// Serializes a user-key EXPORT or IMPORT (#85 ask 2).
    ///
    /// The import is a read-modify-write over a key file with no other guard, and control requests
    /// dispatch concurrently (#172). Two concurrent `replace` imports could otherwise leave the
    /// file holding one key while the node PRESENTS another — both answering `Ok` with different
    /// `user_id`s, and the divergence surfacing only at the next restart. The export shares the
    /// lock so it cannot read a half-replaced file.
    ///
    /// #221: it is also the lock the #86 gate is checked UNDER. `peer_endorse`, `device_revoke`,
    /// `user_key_export` and `inviter_ctx`'s `sign_binding` read `adopted_binding` after taking
    /// it and hold it through reading the key and signing — or, for the export, building the
    /// phrase (`handlers::lock_own_user_key`), so an
    /// `adopt_hook` or `user_key_import` — which write under it — cannot land between the check and
    /// the signature. Held only across file IO on a blocking thread, never a network await.
    pub(crate) user_key_lock: tokio::sync::Mutex<()>,
    /// #221 test seam: runs ONCE inside an identity-signing path, after its #86 checks and before
    /// the key is read — the window an adopt or import must not be able to land in. `None` except
    /// in a test that installs one.
    #[cfg(test)]
    pub(crate) identity_gate_hook: std::sync::Mutex<Option<IdentityGateHook>>,
    /// Embedder-registered ALPNs → their handlers (#67), read by the accept loop's dispatch.
    ///
    /// mcpmesh had built the hard parts of a P2P application platform — identity, pairing, a trust
    /// gate, relay fallback, discovery, a connection registry — and exposed exactly
    /// ONE protocol shape on top: request/response MCP over bi-streams. Anything else (realtime
    /// media wanting datagrams, bulk transfer, an app-level overlay) was out of reach no matter how
    /// well the identity layer suited it, and the only alternative was a SECOND endpoint with a
    /// second identity — discarding the gate, the pairing relationship and the relay config, and
    /// making users pair twice.
    ///
    /// **Registered protocols go through the same `gate_and_register` as every built-in arm**, so
    /// they inherit authorization, the connection registry and severing rather than bypassing them.
    /// That is the whole point: a composition of pieces that already exist, not a second door.
    ///
    /// What they do NOT inherit is a rate limit. The pair/ping/app-blob arms each meter admission
    /// (`admit_pair_accept`, `admit_ping`, `admit_blob_conn`); this arm has none, so an AUTHORIZED
    /// peer can churn custom-ALPN connections as fast as it likes — the same shape the MCP arm has,
    /// where the metering is per request rather than per connection. An embedder that needs a bound
    /// imposes it in its own handler.
    ///
    /// `RwLock` because registration is rare and the accept loop reads it per connection. Held only
    /// for the clone-out, never across the handler's `accept`.
    pub(crate) app_protocols: std::sync::RwLock<
        std::collections::HashMap<Vec<u8>, Arc<dyn iroh::protocol::DynProtocolHandler>>,
    >,
    /// The ALPN set boot actually bound on the endpoint (#67), so a re-advertise can restore it
    /// verbatim instead of recomputing one from a signal that may not match. See
    /// [`MeshState::rebind_alpns`].
    pub(crate) bound_alpns: std::sync::OnceLock<Vec<Vec<u8>>>,
    /// EPHEMERAL service registrations (#36): in-memory only, never written to config, torn down
    /// when the registering control connection closes. Keyed by service name → its backend spec +
    /// allow list. Overlaid onto the config-built registry on every hot-reload
    /// ([`build_services_audited`]), so ephemeral services survive concurrent grants/renames.
    /// Mutated only under `reload_lock` (like every other registry change). Lost on restart by
    /// design — an embedder re-registers per boot.
    pub(crate) ephemeral_services:
        std::sync::Mutex<std::collections::HashMap<String, EphemeralService>>,
    /// In-flight `blob_fetch`es, hash → its cancel token + how many fetches share it (#172).
    ///
    /// Keyed by HASH rather than by request, because a hash is the only handle a cancelling caller
    /// can hold: `ControlClient` is one-request-at-a-time, so the cancel necessarily arrives on a
    /// DIFFERENT connection than the fetch, where no per-request identity is in scope. Concurrent
    /// fetches of one hash therefore share one token and cancel together — the semantic a UI with
    /// one progress bar per blob wants.
    ///
    /// Refcounted so the LAST fetch to finish removes the entry: a plain remove-on-finish would let
    /// one completing fetch drop the token a sibling is still watching, and that sibling would
    /// become uncancellable.
    pub(crate) fetches: std::sync::Mutex<std::collections::HashMap<String, FetchSlot>>,
}

/// One hash's in-flight-fetch registration (#172) — see [`MeshState::fetches`].
#[derive(Clone)]
pub struct FetchSlot {
    pub token: crate::cancel::CancelToken,
    /// How many `blob_fetch` calls are currently watching `token`.
    pub waiters: usize,
}

/// One ephemeral (connection-scoped) service registration (#36): the backend to serve and the
/// nicknames/groups admitted to it. The in-memory analogue of a `[services.*]` config entry.
#[derive(Clone)]
pub struct EphemeralService {
    pub backend: mcpmesh_local_api::BackendSpec,
    pub allow: Vec<String>,
    /// Per-service request rate (#63), CLAMPED to `[limits].rate_limit_per_min` when applied.
    ///
    /// Carried on the ephemeral path deliberately: #55 was filed because a per-service feature (the
    /// allow list) silently did nothing for ephemeral registrations, and repeating that shape would
    /// earn the same report.
    pub rate_limit_per_min: Option<u32>,
}

/// Ring depth of the reachability transition fan-out (#58). Transitions are rare relative to audit
/// records — a peer going up or down, not every request — so a shallow ring is ample; a subscriber
/// that still falls behind gets the same `Lagged` frame the audit ring uses.
const REACH_BROADCAST_DEPTH: usize = 64;

/// Ring depth for app-blob transfer progress (#82). Deeper than the reachability ring: a transfer
/// emits ~102 coalesced frames over its life and several can be in flight, so a shallow ring would
/// make a subscriber that pauses briefly miss the middle of a transfer.
const BLOB_BROADCAST_DEPTH: usize = 256;

/// One coalesced app-blob transfer observation (#82), as it rides [`MeshState::blob_bcast`].
///
/// Carries the STABLE principal, never a display nickname (#38) — `control` maps it to
/// `StreamFrame::BlobTransfer` unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobTransfer {
    pub direction: mcpmesh_local_api::BlobDirection,
    pub hash: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub state: mcpmesh_local_api::BlobTransferState,
    pub peer: Option<String>,
}

/// Cap on the [`MeshState::recent_pairings`] ring: enough for a burst of ceremonies (a person
/// pairing several devices back-to-back) while keeping `status` output and memory tiny.
const RECENT_PAIRINGS_CAP: usize = 8;

impl MeshState {
    /// Assemble the mesh half from its parts, wrapped in an `Arc` (it is always shared —
    /// held by [`DaemonState`] AND by the running accept loop). `accept_task` starts empty;
    /// the caller spawns the loop with the returned `Arc` and installs the handle via
    /// [`set_accept_task`](Self::set_accept_task) (the construction chicken-egg: the loop
    /// needs `mesh`, and `mesh.accept_task` needs the loop's handle).
    ///
    /// `pub` so integration tests can build one; the fields stay `pub(crate)`.
    // The mesh half genuinely has 12 collaborators to assemble (endpoint, gate, store, invites,
    // nickname, config, roster, registry — plus roster mode's gossip/blobs handles + the two topic
    // subscriptions); a params-struct would only rename the same fields, and this signature is
    // pinned by the integration tests that assemble hermetic meshes. The four roster-transport
    // params are `None`/empty in a pure-pairing daemon.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        endpoint: iroh::Endpoint,
        gate: Arc<dyn TrustGate>,
        store: Arc<PeerStore>,
        invites: Arc<LiveInvites>,
        self_nickname: String,
        config_path: PathBuf,
        roster: Arc<RosterGate>,
        conn_registry: Arc<ConnRegistry>,
        gossip: Option<iroh_gossip::net::Gossip>,
        blobs: Option<crate::roster::transport::RosterBlobs>,
        roster_topic: Option<crate::roster::transport::RosterGossip>,
        presence_topic: Option<crate::roster::transport::RosterGossip>,
    ) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            gate,
            store,
            invites,
            self_nickname: std::sync::RwLock::new(self_nickname),
            accept_task: tokio::sync::Mutex::new(None),
            poll_loop: tokio::sync::Mutex::new(None),
            reload_lock: tokio::sync::Mutex::new(()),
            // Defaults to today's behaviour; `boot` overrides it from `[network].presence_mode`
            // (#89). Set post-construction rather than as a 13th parameter — `new` is pinned by 40+
            // hermetic-mesh call sites across the integration tests, and every one of them wants
            // the default.
            presence_mode: std::sync::RwLock::new(crate::daemon::PresenceMode::default()),
            local_discovery: std::sync::OnceLock::new(),
            admit_attested: std::sync::OnceLock::new(),
            keep_alive_secs: std::sync::OnceLock::new(),
            peer_hooks: std::sync::OnceLock::new(),
            config_path,
            applied_relays: std::sync::Mutex::new(RelayPosture::default()),
            roster,
            conn_registry,
            // Empty until `spawn_accept_loop` installs the built registry; nothing serves before
            // then, so an empty live handle is never read.
            sever_observer: std::sync::Mutex::new(None),
            services: Arc::new(mcpmesh_net::LiveServices::new(Arc::new(
                mcpmesh_net::Services::new(std::collections::HashMap::new()),
            ))),
            gossip,
            blobs,
            roster_topic: tokio::sync::Mutex::new(roster_topic),
            presence_topic: Arc::new(tokio::sync::Mutex::new(presence_topic)),
            presence_table: Arc::new(crate::roster::presence::PresenceTable::new()),
            app_metadata: Arc::new(std::sync::RwLock::new(String::new())),
            app_blobs: tokio::sync::Mutex::new(None),
            blobs_dir: std::sync::OnceLock::new(),
            blobs_gc: std::sync::OnceLock::new(),
            audit: std::sync::OnceLock::new(),
            limits: std::sync::OnceLock::new(),
            roster_addr_book: std::sync::OnceLock::new(),
            self_binding: std::sync::OnceLock::new(),
            user_key_path: std::sync::OnceLock::new(),
            user_key_still_boot_minted: std::sync::atomic::AtomicBool::new(false),
            imported_binding: std::sync::RwLock::new(None),
            adopted_binding: std::sync::RwLock::new(None),
            recent_pairings: std::sync::Mutex::new(std::collections::VecDeque::new()),
            reachability: std::sync::Mutex::new(std::collections::HashMap::new()),
            identity_conflict: std::sync::OnceLock::new(),
            reach_bcast: tokio::sync::broadcast::channel(REACH_BROADCAST_DEPTH).0,
            blob_bcast: tokio::sync::broadcast::channel(BLOB_BROADCAST_DEPTH).0,
            // Same depth as the reachability ring: posture transitions are rarer still.
            self_net_bcast: tokio::sync::broadcast::channel(REACH_BROADCAST_DEPTH).0,
            resume_bcast: tokio::sync::broadcast::channel(REACH_BROADCAST_DEPTH).0,
            self_net_change: std::sync::Mutex::new(None),
            probe_seq: std::sync::atomic::AtomicU64::new(0),
            probes_inflight: std::sync::Mutex::new(std::collections::HashSet::new()),
            conn_cache: conn_cache::McpConnCache::new(),
            org_author_lock: tokio::sync::Mutex::new(()),
            user_key_lock: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            identity_gate_hook: std::sync::Mutex::new(None),
            app_protocols: std::sync::RwLock::new(std::collections::HashMap::new()),
            bound_alpns: std::sync::OnceLock::new(),
            ephemeral_services: std::sync::Mutex::new(std::collections::HashMap::new()),
            fetches: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// The mesh's iroh endpoint.
    ///
    /// `#[doc(hidden)]` — a TEST SEAM. The #64 path test dials a peer itself to inspect the path
    /// set the daemon's own probe sees, which is the only way to assert that a `Direct` verdict
    /// came from the SELECTED path rather than from an open relay standby.
    #[doc(hidden)]
    pub fn endpoint_for_test(&self) -> &iroh::Endpoint {
        &self.endpoint
    }

    /// Adopt the shared duplicate-identity cell (#134). Set-once; a second call is ignored, so a
    /// host that both passes one to `NodeBuilder` and runs the daemon path cannot end up with the
    /// status projection reading a different cell than the layer writes.
    pub(crate) fn adopt_identity_conflict(&self, shared: Arc<crate::diag::IdentityConflict>) {
        let _ = self.identity_conflict.set(shared);
    }

    /// When another endpoint was last seen presenting this node's identity (#134), or `None` —
    /// which covers BOTH "never observed" and "no detector installed here". Never render it as
    /// "this identity is unique".
    pub(crate) fn identity_conflict_epoch(&self) -> Option<i64> {
        self.identity_conflict.get()?.last_seen_epoch()
    }

    /// The reachability broadcast, for subscribing BEFORE the event under test can occur.
    ///
    /// `#[doc(hidden)]` — a TEST SEAM (#92 item 2). The live-path suite must subscribe before it
    /// opens the session, or the transition it exists to observe can land in the gap between open
    /// and subscribe and the test passes or fails on timing rather than on behaviour.
    #[doc(hidden)]
    /// The app-blob transfer ring (#82), so an integration test can assert that frames produced by
    /// the provider actually REACH a subscriber. Without it nothing pins the wiring: deleting the
    /// `blob_frame` mapping passed the whole workspace.
    pub fn blob_bcast_for_test(&self) -> &tokio::sync::broadcast::Sender<BlobTransfer> {
        &self.blob_bcast
    }

    pub fn reach_bcast_for_test(&self) -> &tokio::sync::broadcast::Sender<ReachTransition> {
        &self.reach_bcast
    }

    /// Drive ONE suspend-watcher tick with supplied clock deltas (#167 ask 2).
    ///
    /// `#[doc(hidden)]` — a TEST SEAM. The watcher's own loop reads two real clocks, and staging a
    /// genuine suspend would mean suspending the machine running the suite; this hands a test the
    /// deltas so the detection AND the broadcast are exercised for real. Returns what was sent, or
    /// `None` if the deltas were an ordinary tick.
    #[doc(hidden)]
    pub fn resume_tick_for_test(&self, deltas: ClockDeltas, now_wall: i64) -> Option<ResumeEvent> {
        resume::tick(self, deltas, now_wall)
    }

    /// The current probe ticket counter.
    ///
    /// `#[doc(hidden)]` — a TEST SEAM (#92 item 2). Reading it before and after proves a frame came
    /// from a LIVE session rather than from a probe: probe-driven path frames are item (1), shipped
    /// in 0.19.0, so a test that cannot tell the two apart proves nothing about item (2).
    #[doc(hidden)]
    pub fn probe_seq_for_test(&self) -> u64 {
        self.probe_seq.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Install an EPHEMERAL service registration directly (#36's in-memory map).
    ///
    /// `pub` (like [`MeshState::new`] and [`spawn_accept_loop`]) so integration tests can stand up
    /// an ephemeral service without driving a full control connection, and exercise the SAME
    /// grant/revoke routing the verbs use. Returns the entry it replaced, if any.
    ///
    /// **`#[doc(hidden)]` — a TEST SEAM, not the registration API.** It deliberately bypasses
    /// everything `register_service { ephemeral: true }` enforces: it takes no `reload_lock`,
    /// triggers no reload (the live registry stays stale until something else swaps), and skips the
    /// config-collision check that keeps a name from being held ephemerally AND persistently at
    /// once. Use `register_service`.
    #[doc(hidden)]
    /// Install the sever observer (#99). Test seam; see the field docs.
    #[doc(hidden)]
    pub fn set_sever_observer<F>(&self, f: F)
    where
        F: Fn(&mcpmesh_net::Services) + Send + Sync + 'static,
    {
        *self
            .sever_observer
            .lock()
            .expect("sever observer lock not poisoned") = Some(Arc::new(f));
    }

    /// The LIVE service registry as of now — the same handle the accept path reads per accepted
    /// bi-stream. A test seam: it lets a test assert what the registry admits at a precise instant
    /// (e.g. that a revoke's swap is installed before it severs) without racing the wire.
    #[doc(hidden)]
    pub fn live_services(&self) -> Arc<mcpmesh_net::Services> {
        self.services.get()
    }

    pub fn register_ephemeral(
        &self,
        name: String,
        service: EphemeralService,
    ) -> Option<EphemeralService> {
        self.ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned")
            .insert(name, service)
    }

    /// Add `principal` to an EPHEMERAL service's in-memory allow (#55).
    ///
    /// `None` when no ephemeral registration carries that name — the caller then falls through to
    /// the config writers. `Some(changed)` when it exists, `changed` reporting whether the allow
    /// actually moved, so an idempotent re-grant causes no reload (the same contract
    /// [`append_allow_to_config`](crate::daemon::config_write::append_allow_to_config) has).
    ///
    /// An ephemeral registration's `allow` lives only here, so before this the grant verb edited
    /// `config.toml`, found no entry, and reported success while admitting nobody.
    ///
    /// The std `Mutex` is held for the lookup + push only, never across an await; the CALLER holds
    /// `reload_lock` around the whole mutate→reload→swap section, as every registry change does.
    pub(crate) fn grant_ephemeral(&self, service: &str, principal: &str) -> Option<bool> {
        let mut map = self
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned");
        let entry = map.get_mut(service)?;
        if entry.allow.iter().any(|a| a == principal) {
            return Some(false);
        }
        entry.allow.push(principal.to_string());
        Some(true)
    }

    /// Remove `principal` from an EPHEMERAL service's in-memory allow (#69) — the exact inverse of
    /// [`grant_ephemeral`](Self::grant_ephemeral), with the same `None`/`Some(changed)` contract.
    ///
    /// Before this, revoking against an ephemeral service stripped `config.toml` (which never held
    /// the entry) and the next hot-reload re-overlaid the untouched in-memory allow, so the peer
    /// stayed admitted.
    pub(crate) fn revoke_ephemeral(&self, service: &str, principal: &str) -> Option<bool> {
        let mut map = self
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned");
        let entry = map.get_mut(service)?;
        let before = entry.allow.len();
        entry.allow.retain(|a| a != principal);
        Some(entry.allow.len() != before)
    }

    /// Record a completed inviter-side pairing for the `status` ceremony surface (display-only —
    /// see the [`recent_pairings`](Self::recent_pairings) field doc). Bounded: the OLDEST entry
    /// is dropped once the ring holds [`RECENT_PAIRINGS_CAP`]. The whole row, so the ceremony
    /// that ran (`self_enroll`, #214) is fixed where it is known and not re-derived here.
    pub(crate) fn record_pairing(&self, entry: mcpmesh_local_api::RecentPairing) {
        let mut ring = self
            .recent_pairings
            .lock()
            .expect("recent_pairings lock not poisoned");
        if ring.len() >= RECENT_PAIRINGS_CAP {
            ring.pop_front();
        }
        ring.push_back(entry);
    }

    /// Snapshot of the recent inviter-side pairings, NEWEST FIRST (the order `status` renders —
    /// the code the human is looking for is almost always the latest one). `pub` as a test seam
    /// (#214): the two-node ceremony tests assert which ceremony each row records.
    #[doc(hidden)]
    pub fn recent_pairings(&self) -> Vec<mcpmesh_local_api::RecentPairing> {
        self.recent_pairings
            .lock()
            .expect("recent_pairings lock not poisoned")
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    /// Install the gated app-blob provider post-construction (both trust modes, #61).
    /// Called by `serve_forever` BEFORE `spawn_accept_loop`, so the `APP_BLOB_ALPN` arm always sees
    /// it once serving begins.
    pub async fn set_app_blobs(&self, provider: Arc<crate::blobs::provider::AppBlobs>) {
        *self.app_blobs.lock().await = Some(provider);
    }

    /// A clone of the installed app-blob provider handle, or `None` (pure-pairing / not yet set).
    pub async fn app_blobs(&self) -> Option<Arc<crate::blobs::provider::AppBlobs>> {
        self.app_blobs.lock().await.clone()
    }

    /// Install the process audit sink, once, before serving. A second call is ignored
    /// (`OnceLock::set` returns `Err`), keeping the invariant self-healing.
    /// The name a freshly minted invite would present — read-clone (the lock is
    /// never held across an await).
    pub(crate) fn self_nickname(&self) -> String {
        self.self_nickname
            .read()
            .expect("self_nickname lock not poisoned")
            .clone()
    }

    /// Install a new self-nickname (the `set_nickname` verb, #37) — called only AFTER the
    /// config write succeeded, so the in-memory name never runs ahead of the persisted one.
    /// This node's current app metadata (#39) — read-clone (the lock is never held across an
    /// await). Empty when unset.
    pub(crate) fn app_metadata(&self) -> String {
        self.app_metadata
            .read()
            .expect("app_metadata lock not poisoned")
            .clone()
    }

    /// Register an embedder protocol on `alpn` (#67), replacing any handler already on it, and
    /// rebind the endpoint's advertised ALPN set so peers can negotiate it.
    ///
    /// Returns an error rather than registering when `alpn` is empty or begins with `mcpmesh/`.
    /// That prefix is RESERVED: the accept loop dispatches the built-in arms by exact ALPN, and
    /// this registry is consulted only for ALPNs it does not own — so a handler on `mcpmesh/mcp/1`
    /// would be silently dead rather than dangerous, and one on a `mcpmesh/*` name mcpmesh adds
    /// LATER would flip from working to dead on an upgrade. Refusing the namespace makes both
    /// impossible. `app/*` is the suggested convention for embedder ALPNs.
    ///
    /// **Takes effect for connections negotiated from now on.** ALPN is chosen at handshake, so a
    /// peer that connected before this call cannot use the new protocol — register during startup,
    /// before announcing the node as ready, if that matters.
    pub(crate) fn register_app_protocol(
        &self,
        alpn: &[u8],
        handler: Arc<dyn iroh::protocol::DynProtocolHandler>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!alpn.is_empty(), "an ALPN must not be empty");
        // Refused against the ACTUAL built-in set, not just the `mcpmesh/` prefix. Two of the six
        // built-in arms are NOT in that namespace — `/iroh-gossip/1` and `/iroh-bytes/4`, whose
        // names iroh-gossip and iroh-blobs own — so a prefix check let an embedder register on
        // them. Both arms sit above the app arm in the dispatch, so the handler was accepted,
        // advertised, and silently dead; on a pairing-mode node it was worse, because the
        // registration ADDED the ALPN to the advertised set and the peer then negotiated into a
        // "gossip not enabled" close. Caught by review, by execution.
        //
        // `alpns_for(true)` is the SUPERSET (roster mode), so the refusal does not depend on how
        // this particular node booted: an ALPN that would collide on any node is refused on every
        // node, and an embedder cannot write code that works in pairing mode and breaks in roster
        // mode.
        anyhow::ensure!(
            !crate::daemon::boot::alpns_for(true)
                .iter()
                .any(|a| a == alpn),
            "that ALPN is one of mcpmesh's own protocols and is dispatched before this registry, \
             so a handler on it would never run"
        );
        // …and the namespace is reserved on top, so a protocol mcpmesh adds LATER cannot turn a
        // working registration into a dead one on upgrade.
        anyhow::ensure!(
            !alpn.starts_with(b"mcpmesh/"),
            "the `mcpmesh/` ALPN namespace is reserved for mcpmesh's own protocols; use your own \
             prefix (`app/…` by convention)"
        );
        {
            let mut map = self
                .app_protocols
                .write()
                .expect("app_protocols lock not poisoned");
            map.insert(alpn.to_vec(), handler);
        }
        self.rebind_alpns();
        Ok(())
    }

    /// Re-advertise the endpoint's ALPN set: the built-ins this node booted with, plus every
    /// registered embedder protocol (#67).
    ///
    /// `set_alpns` REPLACES the whole set, so the built-ins must be included every time —
    /// appending to a partial list is how an endpoint quietly stops answering its own protocols.
    ///
    /// It reads the set boot ACTUALLY BOUND rather than recomputing one. The first version
    /// recomputed via `alpns_for(self.roster_transport_live())`, which is a DIFFERENT signal from
    /// the `roster_mode` boot used (`org_root_pk.is_some()` vs `gossip.is_some()`); they diverge
    /// exactly where this file already documents — roster mode on, no `org_id` resolvable — and a
    /// registration there silently NARROWED the advertised set. Measured, in review. Asking what
    /// was bound cannot drift from what was bound.
    ///
    /// Falls back to `alpns_for(true)`, the superset, if boot never recorded a set. That direction
    /// is deliberate: advertising a roster ALPN a node cannot serve costs a clean close, while
    /// dropping one costs the protocol.
    ///
    /// **The read guard is held across `set_alpns` on purpose.** It blocks a concurrent
    /// registration's insert, so two racing registrations produce strictly ordered `set_alpns`
    /// calls and the later one always sees the fuller map. Releasing it early looks tidier and
    /// reintroduces a lost update.
    fn rebind_alpns(&self) {
        let mut alpns = self
            .bound_alpns
            .get()
            .cloned()
            .unwrap_or_else(|| crate::daemon::boot::alpns_for(true));
        let map = self
            .app_protocols
            .read()
            .expect("app_protocols lock not poisoned");
        alpns.extend(map.keys().cloned());
        self.endpoint.set_alpns(alpns);
    }

    /// What  would advertise right now — test-only, so the never-narrow property is
    /// checked against the actual bytes rather than through a live handshake.
    #[cfg(test)]
    pub(crate) fn advertised_alpns_for_test(&self) -> Vec<Vec<u8>> {
        let mut alpns = self
            .bound_alpns
            .get()
            .cloned()
            .unwrap_or_else(|| crate::daemon::boot::alpns_for(true));
        let map = self
            .app_protocols
            .read()
            .expect("app_protocols lock not poisoned");
        alpns.extend(map.keys().cloned());
        alpns
    }

    /// Record the ALPN set boot bound on the endpoint (#67). Boot calls this once; nothing else
    /// should. See [`rebind_alpns`](Self::rebind_alpns) for why it is recorded rather than
    /// recomputed.
    pub(crate) fn set_bound_alpns(&self, alpns: Vec<Vec<u8>>) {
        let _ = self.bound_alpns.set(alpns);
    }

    /// The handler registered for `alpn`, if any (#67). Cloned out — the lock is never held across
    /// the handler's `accept`, which is long-running by design.
    pub(crate) fn app_protocol(
        &self,
        alpn: &[u8],
    ) -> Option<Arc<dyn iroh::protocol::DynProtocolHandler>> {
        self.app_protocols
            .read()
            .expect("app_protocols lock not poisoned")
            .get(alpn)
            .cloned()
    }

    /// Is the roster TRANSPORT actually composed on this process (#93b)?
    ///
    /// `roster_mode` is a boot-time snapshot of "an org root is pinned", and it decides two things
    /// nothing can change afterwards: the ALPN set bound on the endpoint, and whether gossip,
    /// presence and app-blobs are constructed at all. The roster GATE, by contrast, hot-swaps live.
    ///
    /// So a node that booted in PAIRING mode and then ran `org_join` is half-live: MCP sessions to
    /// org members start working the moment a roster arrives, while presence stays permanently
    /// empty and blobs hard-close with `blobs not enabled`. Partial success with no error is the
    /// worst failure shape available, and an embedder had no way to detect it — which is what
    /// `OrgJoinResult::restart_required` now answers.
    ///
    /// **Derived from the composed transport, not from `roster_mode`.** They differ in a case that
    /// matters: `compose_roster_transport` also returns nothing when roster mode is on but no
    /// `org_id` is known (it warns and disables gossip). The ALPNs are bound in that case and
    /// presence still does not work, so `roster_mode` would report "live" for a node that is not.
    /// Asking the transport itself cannot drift from what was actually built. `blobs` is composed
    /// in the same branch as `gossip`, so one answers for both.
    pub(crate) fn roster_transport_live(&self) -> bool {
        self.gossip.is_some()
    }

    /// This node's `[network].local_discovery` spelling (#68), for `status.self_network`.
    /// Defaults to `"off"` — the config default, and the answer for a mesh boot never touched.
    pub fn local_discovery(&self) -> &str {
        self.local_discovery.get().map_or("off", String::as_str)
    }

    /// Install the boot-resolved `[network].local_discovery` spelling (#68).
    pub fn set_local_discovery(&self, mode: String) {
        let _ = self.local_discovery.set(mode);
    }

    /// This node's EFFECTIVE QUIC keepalive interval in seconds (#166) — `[network].keep_alive_secs`
    /// if set, else iroh's default.
    ///
    /// Read by the per-connection idle-timeout check, which refuses a value at or below it: a
    /// keepalive arriving after the idle timer has fired severs a session whose peer is alive.
    /// Stored at boot alongside the other transport knobs rather than re-read from disk, so the
    /// refusal tracks what this endpoint was actually built with.
    pub fn keep_alive_secs(&self) -> u64 {
        self.keep_alive_secs
            .get()
            .copied()
            // iroh's DEFAULT keepalive, which happens to equal its per-path CAP. Named by the cap
            // constant because the tree has only that one — the gate rightly flagged that
            // `boot.rs` calls it "not a default, a cap". They are both 5s today, and
            // `iroh_transport_defaults_are_what_the_docs_claim` pins the default, so that test
            // fails first if they ever diverge.
            .unwrap_or(crate::daemon::boot::IROH_MAX_PATH_KEEP_ALIVE_SECS)
    }

    /// Install the boot-resolved effective keepalive (#166).
    pub fn set_keep_alive_secs(&self, secs: u64) {
        let _ = self.keep_alive_secs.set(secs);
    }

    /// #85 ask 3: does this node admit attested devices of people it already pairs with?
    /// `[identity].admit_attested_devices`, resolved at boot. Defaults to FALSE — a node whose boot
    /// never set it must not silently widen admission.
    pub fn admit_attested_devices(&self) -> bool {
        self.admit_attested.get().copied().unwrap_or(false)
    }

    /// Install the boot-resolved `[identity].admit_attested_devices` (#85 ask 3).
    pub fn set_admit_attested_devices(&self, on: bool) {
        let _ = self.admit_attested.set(on);
    }

    /// Who currently gets a reachability pong (#89). Read on every `mcpmesh/ping/1` accept.
    pub fn presence_mode(&self) -> crate::daemon::PresenceMode {
        *self
            .presence_mode
            .read()
            .expect("presence_mode lock not poisoned")
    }

    /// Install the boot-resolved `[network].presence_mode` (#89).
    ///
    /// The supported way to set this is `[network].presence_mode` in config, which `boot` reads
    /// once. This setter exists because `boot` installs it post-construction (`MeshState::new` is
    /// pinned by 40+ hermetic-mesh call sites) and because the accept-arm tests drive it directly.
    ///
    /// Note for embedders: reaching this requires a `MeshState`, and `Node::mesh()` is private —
    /// which is #89's own observation about there being no embedder interception point. A runtime
    /// "appear offline" toggle therefore needs that seam opened first; it is not part of this
    /// change.
    pub fn set_presence_mode(&self, mode: crate::daemon::PresenceMode) {
        *self
            .presence_mode
            .write()
            .expect("presence_mode lock not poisoned") = mode;
    }

    /// Set this node's app metadata (#39); future heartbeats carry it. `""` clears it.
    pub(crate) fn set_app_metadata(&self, metadata: String) {
        *self
            .app_metadata
            .write()
            .expect("app_metadata lock not poisoned") = metadata;
    }

    pub(crate) fn set_self_nickname(&self, nickname: String) {
        *self
            .self_nickname
            .write()
            .expect("self_nickname lock not poisoned") = nickname;
    }

    /// Seed / update the live relay posture (#53). Called at boot from `[network]` and after each
    /// successful LIVE `set_relays`.
    pub(crate) fn set_applied_relays(&self, mode: &str, urls: &[String]) {
        *self
            .applied_relays
            .lock()
            .expect("applied_relays lock not poisoned") = RelayPosture {
            mode: mode.to_string(),
            urls: urls.to_vec(),
        };
    }

    /// The relay posture currently applied to the live endpoint — the runtime "current set" the
    /// `set_relays` verb (#53) diffs a desired set against.
    pub(crate) fn applied_relays(&self) -> RelayPosture {
        self.applied_relays
            .lock()
            .expect("applied_relays lock not poisoned")
            .clone()
    }

    pub fn set_audit(&self, sink: AuditSink) {
        let _ = self.audit.set(sink);
    }

    /// Install the endpoint hooks this mesh's endpoint was built with (#229), once, at boot.
    pub(crate) fn set_peer_hooks(&self, h: hooks::MeshHooks) {
        let _ = self.peer_hooks.set(h);
    }

    /// Close every live connection this node DIALLED to a device it now refuses to
    /// dial (#229). Every revoke path calls it AFTER its write. Returns how many were closed; `0` on
    /// a mesh with no hooks installed.
    pub(crate) async fn close_refused_peer_conns(&self) -> usize {
        let Some(h) = self.peer_hooks.get() else {
            return 0;
        };
        let Some(gate) = h.gate() else {
            return 0;
        };
        hooks::close_refused(&h.conns(), &gate).await
    }

    /// Record the on-disk app-blob store directory, once, at boot (#88) — alongside
    /// [`set_app_blobs`](Self::set_app_blobs), so `status.storage.blobs_bytes` can walk it
    /// synchronously. Never set on a node without an on-disk blob store (blobs_bytes reads 0).
    pub fn set_blobs_dir(&self, dir: PathBuf) {
        let _ = self.blobs_dir.set(dir);
    }

    /// The recorded blob-store directory, if this node has one.
    pub(crate) fn blobs_dir(&self) -> Option<&Path> {
        self.blobs_dir.get().map(PathBuf::as_path)
    }

    /// Record that blob GC is RUNNING, at `interval_secs`, with these counters (#80). Called at
    /// boot only when collection was actually configured — an unset cell is what makes
    /// `status.storage.blobs_gc` read `None` rather than "configured, zero runs".
    pub fn set_blobs_gc(
        &self,
        interval_secs: u64,
        stats: Arc<crate::blobs::provider::BlobGcStats>,
    ) {
        let _ = self.blobs_gc.set((interval_secs, stats));
    }

    /// The audit sink, or the disabled no-op sink if none was installed (control-only test daemon).
    pub(crate) fn audit(&self) -> AuditSink {
        self.audit.get().cloned().unwrap_or_default()
    }

    /// Where an ADOPTED self-enrollment binding is persisted (#86) — beside the user key, so a
    /// `--profile` root keeps its own.
    pub(crate) fn adopted_binding_path(&self) -> PathBuf {
        self.user_key_path
            .get()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("user.key"))
            .with_extension("adopted-binding.json")
    }

    /// Install an adopted binding LIVE (#86), so an enrolled device presents the shared identity
    /// without a restart.
    pub(crate) fn set_self_binding_live(
        &self,
        binding: Option<crate::pairing::rendezvous::SelfBinding>,
    ) {
        *self
            .adopted_binding
            .write()
            .expect("adopted_binding lock not poisoned") = binding;
    }

    /// Install a binding restored from a recovery phrase (#85 ask 2), and SUPERSEDE any enrollment.
    ///
    /// Clearing the adopted slot is not tidiness. That slot means "this device holds no user key",
    /// and after an import it does — leaving it set makes `peer_endorse` and `invite --as-self`
    /// refuse on a machine that has just recovered its own identity, which is the machine most
    /// likely to need them.
    pub(crate) fn set_imported_binding(&self, binding: crate::pairing::rendezvous::SelfBinding) {
        *self
            .imported_binding
            .write()
            .expect("imported_binding lock not poisoned") = Some(binding);
        *self
            .adopted_binding
            .write()
            .expect("adopted_binding lock not poisoned") = None;
    }

    /// Fire the #221 test seam, if one is installed. A no-op outside tests.
    pub(crate) async fn run_identity_gate_hook(&self) {
        #[cfg(test)]
        {
            let hook = self
                .identity_gate_hook
                .lock()
                .expect("identity_gate_hook lock not poisoned")
                .take();
            if let Some(hook) = hook {
                hook().await;
            }
        }
    }

    /// Boot (#85 ask 2): record whether it MINTED the user key rather than loading one.
    pub(crate) fn note_user_key_minted_at_boot(&self, created: bool) {
        self.user_key_still_boot_minted
            .store(created, std::sync::atomic::Ordering::SeqCst);
    }

    /// An import wrote a key over the file (#221): what is on disk now is a real identity, and the
    /// `replace` guard must defend it for the rest of this lifetime.
    pub(crate) fn note_user_key_replaced(&self) {
        self.user_key_still_boot_minted
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Is the user key on disk still the one this boot minted (#85 ask 2, #221)?
    pub(crate) fn user_key_still_boot_minted(&self) -> bool {
        self.user_key_still_boot_minted
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Install the resolved `UserKey` path (#65). Set once, at boot, like `self_binding`.
    pub fn set_user_key_path(&self, path: PathBuf) {
        let _ = self.user_key_path.set(path);
    }

    /// Install this daemon's self-sovereign pairing identity, once, before serving (like
    /// [`set_audit`](Self::set_audit)). `None` records "this daemon has no user key" explicitly.
    pub fn set_self_binding(&self, binding: Option<crate::pairing::rendezvous::SelfBinding>) {
        let _ = self.self_binding.set(binding);
    }

    /// A clone of this daemon's self-sovereign pairing identity, or `None` when unset (control-only /
    /// test daemon) or when this daemon has no user key. The pairing handlers present it to peers.
    pub(crate) fn self_binding(&self) -> Option<crate::pairing::rendezvous::SelfBinding> {
        // The most recent explicit act wins. An ADOPTED binding outranks everything (#86, #214):
        // this device was enrolled into another device's identity, so presenting its own — imported
        // or boot-derived — would resolve it to a stranger again. An import CLEARS this slot
        // (`set_imported_binding`), so the two never both apply; an adoption after an import does
        // not clear the imported slot but outranks it, so `self_enroll_detach` falls back to the
        // identity whose key is actually on disk rather than the pre-import boot one.
        if let Some(adopted) = self
            .adopted_binding
            .read()
            .expect("adopted_binding lock not poisoned")
            .clone()
        {
            return Some(adopted);
        }
        // An IMPORTED key wins over the boot-derived one (#85 ask 2): the `OnceLock` cannot be
        // reset, and the key file on disk is now the imported one.
        if let Some(imported) = self
            .imported_binding
            .read()
            .expect("imported_binding lock not poisoned")
            .clone()
        {
            return Some(imported);
        }
        self.self_binding.get().cloned().flatten()
    }

    pub fn set_limits(&self, limits: Arc<crate::limits::MeshLimiters>) {
        let _ = self.limits.set(limits);
    }

    pub(crate) fn limits(&self) -> Arc<crate::limits::MeshLimiters> {
        self.limits
            .get()
            .cloned()
            .unwrap_or_else(crate::limits::MeshLimiters::unlimited)
    }

    pub(crate) fn roster_addr_book(
        &self,
    ) -> Option<std::sync::Arc<crate::roster::transport::RosterAddrBook>> {
        self.roster_addr_book.get().cloned()
    }

    /// A clone of the roster-topic gossip SENDER, or `None` in a pure-pairing daemon. Cloned
    /// under the mutex so `announce_roster` can broadcast from any site while the receiver has
    /// been moved out by the converge loop — the sender stays live in `roster_topic`.
    pub async fn roster_topic_sender(&self) -> Option<iroh_gossip::api::GossipSender> {
        self.roster_topic
            .lock()
            .await
            .as_ref()
            .map(|g| g.sender.clone())
    }

    /// Move the roster-topic gossip RECEIVER out — EXACTLY ONCE, for the distribution receive
    /// loop (a `GossipReceiver` is a single-consumer stream). Leaves the sender in place so
    /// `roster_topic_sender` still announces. `None` if pure-pairing or already taken.
    pub async fn take_roster_topic_receiver(&self) -> Option<iroh_gossip::api::GossipReceiver> {
        self.roster_topic
            .lock()
            .await
            .as_mut()
            .and_then(|g| g.receiver.take())
    }

    /// The narrow context the presence loops run against (`roster::presence::publish_loop` /
    /// `track_loop`): the presence table, the presence-topic handle, and the roster gate — the
    /// SAME `Arc`s this struct holds, so the loops observe live roster hot-swaps without ever
    /// seeing the rest of the daemon.
    pub(crate) fn presence_ctx(&self) -> crate::roster::presence::PresenceCtx {
        crate::roster::presence::PresenceCtx {
            roster: self.roster.clone(),
            table: self.presence_table.clone(),
            topic: self.presence_topic.clone(),
            app_metadata: self.app_metadata.clone(),
        }
    }

    /// The narrow context the inviter-side pair rendezvous runs against: the peer store + invite
    /// ring + this daemon's identity presentation, plus two hooks that reach back into the mesh —
    /// `grant` (the config-append + reload machinery behind [`grant_service_access`], which may
    /// abort/respawn the very accept loop that spawned the handler; safe because the handler is a
    /// detached child task — see the `InviterCtx` doc) and `record_pairing` (the `status`
    /// ceremony ring). Assembled per accepted pair connection by the accept loop.
    pub(crate) fn inviter_ctx(self: &Arc<Self>) -> crate::pairing::rendezvous::InviterCtx {
        let grant_mesh = self.clone();
        let record_mesh = self.clone();
        // ONE snapshot, shared by what the ceremony PRESENTS and what `sign_binding` checks it is
        // still signing for (#221).
        let self_binding = self.self_binding();
        crate::pairing::rendezvous::InviterCtx {
            // #85 ask 3 — read once at boot, like `presence_mode`. Off unless asked for.
            admit_attested: self.admit_attested_devices(),
            self_endpoint_id: *self.endpoint.id().as_bytes(),
            self_nickname: self.self_nickname(),
            store: self.store.clone(),
            invites: self.invites.clone(),
            config_path: self.config_path.clone(),
            self_binding: self_binding.clone(),
            grant: Box::new(move |principal, nickname, services| {
                let mesh = grant_mesh.clone();
                Box::pin(async move {
                    grant_service_access(&mesh, &principal, &nickname, &services).await
                })
            }),
            record_pairing: Box::new(move |entry| record_mesh.record_pairing(entry)),
            // #86: sign a binding for ANOTHER DEVICE of this person. Loads the key per call rather
            // than holding it, matching `peer_endorse`. `None` when this daemon has no user key —
            // there is then no identity to enroll into.
            audit_trust: {
                let mesh = self.clone();
                Box::new(move |event: String, target: Option<String>| {
                    mesh.audit().record(crate::audit::AuditRecord::trust(
                        crate::audit::now_ts(),
                        event,
                        target,
                        None,
                    ));
                })
            },
            sign_binding: {
                let mesh = self.clone();
                let presented = self_binding;
                Box::new(move |endpoint_id: [u8; 32]| {
                    let mesh = mesh.clone();
                    let presented = presented.clone();
                    Box::pin(async move {
                        mesh.sign_ceremony_binding(presented.as_ref(), &endpoint_id)
                            .await
                            .ok()
                    })
                        as std::pin::Pin<
                            Box<dyn std::future::Future<Output = Option<String>> + Send>,
                        >
                })
            },
        }
    }

    /// `inviter_ctx`'s `sign_binding` (#86, #221): sign a device→user binding for `endpoint_id`
    /// under the identity the ceremony PRESENTS, or say why not.
    ///
    /// Every check runs under `user_key_lock`, held through the key read and the signature, so an
    /// adopt or import cannot land in between:
    /// - an ENROLLED device must not enroll a third ([`SignBindingRefusal::Enrolled`]). Boot always
    ///   mints a LOCAL key, so without this it would sign with that key while presenting the
    ///   adopted identity — bindings for an identity no peer has ever seen.
    /// - the ceremony presents the binding snapshotted when the connection was accepted; an import
    ///   or adoption since then is [`SignBindingRefusal::IdentityChanged`].
    /// - the key on disk must be the one behind the presented `user_pk`
    ///   ([`SignBindingRefusal::KeyMismatch`]). Comparing snapshots cannot see a `user.key`
    ///   replaced underneath the running node; a signature under that key would not verify under
    ///   the `user_pk` the reply carries, and the redeemer would refuse after the single-use invite
    ///   was already burned.
    ///
    /// `Ok` therefore always verifies under `presented.user_pk` for `endpoint_id`. The key is READ,
    /// never minted. No network IO.
    pub(crate) async fn sign_ceremony_binding(
        self: &Arc<Self>,
        presented: Option<&crate::pairing::rendezvous::SelfBinding>,
        endpoint_id: &[u8; 32],
    ) -> Result<String, SignBindingRefusal> {
        let _key_guard = crate::daemon::handlers::lock_own_user_key(self, "sign_binding", "Enroll")
            .await
            .map_err(|_| SignBindingRefusal::Enrolled)?;
        let presented = presented.ok_or(SignBindingRefusal::NoIdentity)?;
        if self.self_binding().as_ref() != Some(presented) {
            tracing::warn!(
                "refusing a self-enrollment: this node's identity changed during the ceremony \
                 (#221)"
            );
            return Err(SignBindingRefusal::IdentityChanged);
        }
        self.run_identity_gate_hook().await;
        let user_key = crate::daemon::handlers::read_user_key(self)
            .await
            .map_err(|_| SignBindingRefusal::NoKey)?;
        if mcpmesh_trust::roster::encode_b64u(&user_key.public_bytes()) != presented.user_pk {
            tracing::warn!(
                "refusing a self-enrollment: the user key on disk is not the one behind the \
                 identity this node presents (#221)"
            );
            return Err(SignBindingRefusal::KeyMismatch);
        }
        Ok(crate::pairing::binding_sig_for(&user_key, endpoint_id))
    }

    /// Record that the installed roster was CONFIRMED current at `now` — the freshness `last_confirmed`
    /// bump. Bumps the LIVE gate (the resolve/sever paths see it on the very next call)
    /// AND persists the instant to the per-node sidecar (`<config_dir>/roster.confirmed`) so a restart
    /// re-arms freshness at the confirmed time rather than instantly degrading. Called from ALL the
    /// confirmation events: a URL poll whose served serial is `>= installed` (even EQUAL — proof of
    /// currency without a serial bump, the only channel that gives it), a gossip-delivered roster
    /// passing validation, and a manual install. The persist is best-effort — a write failure leaves
    /// the in-RAM arm intact (the live decision is correct; only a restart would lose the instant).
    /// Async so the sidecar write runs on a blocking worker (the fs house rule).
    pub(crate) async fn confirm_roster_current(&self, now: i64) {
        // Live gate first — the security-bearing update (resolve/sever consult this immediately).
        self.roster.set_last_confirmed(now);
        // Then persist (best-effort). Derived per-node from `config_path` so two daemons in one process
        // (the multi-node integration tests) keep separate sidecars — mirrors `installed_roster_path`.
        let store = FreshnessStore::new(roster_confirmed_path(&self.config_path));
        match blocking("join roster freshness persist", move || store.store(now)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) | Err(e) => {
                tracing::warn!(%e, "persist roster freshness (in-memory freshness still applied)")
            }
        }
    }

    /// Install the accept-loop handle after [`new`](Self::new) + [`spawn_accept_loop`]
    /// (completes the construction chicken-egg). Also used to seed the handle a later
    /// hot-reload aborts.
    ///
    /// Take-and-abort any prior handle first (mirroring the accept-loop start): a stray second
    /// call would otherwise DROP the previous `JoinHandle` — detaching, not stopping, its loop —
    /// leaving two loops accepting on one endpoint. Latent today (each caller invokes once), but
    /// this keeps the invariant self-healing rather than silently doubling the accept loop.
    pub async fn set_accept_task(&self, handle: JoinHandle<()>) {
        let mut guard = self.accept_task.lock().await;
        if let Some(old) = guard.take() {
            old.abort();
        }
        *guard = Some(handle);
    }
}

/// The mesh state IS the roster-distribution channels' host: the narrow seam
/// `roster::distribute` runs against (endpoint + roster gate + blob transport + topic handles +
/// the install pipeline), implemented here so that module never sees this struct. The install
/// pipeline itself lives in [`roster_install`] (`converge_roster_bytes` — the single-writer
/// converge shared with the manual install).
impl crate::roster::distribute::DistributionHost for MeshState {
    fn endpoint(&self) -> &iroh::Endpoint {
        &self.endpoint
    }

    fn dial_refused(&self, endpoint_id: &[u8; 32]) -> bool {
        dial::dial_refused(self, endpoint_id)
    }

    fn roster(&self) -> &RosterGate {
        &self.roster
    }

    fn blobs(&self) -> Option<&crate::roster::transport::RosterBlobs> {
        self.blobs.as_ref()
    }

    fn gossip_active(&self) -> bool {
        self.gossip.is_some()
    }

    fn installed_roster_path(&self) -> PathBuf {
        roster_install::installed_roster_path(self)
    }

    fn pinned_org_root_pk(&self) -> anyhow::Result<Option<String>> {
        roster_install::mesh_config_org_root_pk(self)
    }

    fn addr_book(&self) -> Option<Arc<crate::roster::transport::RosterAddrBook>> {
        self.roster_addr_book()
    }

    fn roster_topic_sender(
        &self,
    ) -> impl std::future::Future<Output = Option<iroh_gossip::api::GossipSender>> + Send {
        MeshState::roster_topic_sender(self)
    }

    fn take_roster_topic_receiver(
        &self,
    ) -> impl std::future::Future<Output = Option<iroh_gossip::api::GossipReceiver>> + Send {
        MeshState::take_roster_topic_receiver(self)
    }

    fn confirm_roster_current(&self, now: i64) -> impl std::future::Future<Output = ()> + Send {
        MeshState::confirm_roster_current(self, now)
    }

    fn install_roster_bytes(
        &self,
        bytes: &[u8],
        serial: u64,
        channel: &'static str,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        roster_install::converge_roster_bytes(self, bytes, serial, channel)
    }
}

/// Build the `Services` registry from config `[services.*]`: a `run` service becomes a
/// [`SpawnBackend`] (its own concurrency semaphore), a `socket` service a [`SocketBackend`].
/// Backends carry NO identity — that is threaded per-caller through `SessionBackend::run`
/// (the injected identity is per-session, the backend is shared). A malformed service (both
/// or neither backend kind) is logged and skipped rather than failing the whole daemon.
///
/// `pub` so the integration tests can compose the SAME registry wiring the daemon uses
/// against an in-process endpoint (the daemon's own `run()` is a subprocess; the test drives
/// the composition directly to prove config → services → gate → backend → env injection).
pub fn build_services(cfg: &Config) -> Services {
    build_services_audited(
        cfg,
        &AuditSink::disabled(),
        &crate::limits::MeshLimiters::unlimited(),
    )
}

/// Build the service registry, giving every backend its service NAME, the audit sink, and its OWN
/// request limiter — one `Arc` per service, keyed internally by endpoint, so the effective bucket is
/// `(service, endpoint)` and one mount can no longer starve another (#63). The invariant this
/// restates:
/// pre-#63 a peer's AGGREGATE rate across every mount was bounded by `rate_limit_per_min`; now that
/// value bounds a peer's rate PER SERVICE and a per-service entry may only LOWER it. Aggregate is
/// bounded by (services granted) × (their limits) — both operator-chosen, neither peer-influenced.
/// A shared bucket is what let a noisy service starve a quiet one, which is what #63 reports.
pub fn build_services_audited(
    cfg: &Config,
    audit: &AuditSink,
    limiters: &Arc<crate::limits::MeshLimiters>,
) -> Services {
    build_services_with_ephemeral(cfg, audit, limiters, &HashMap::new())
}

/// [`build_services_audited`] plus an overlay of EPHEMERAL registrations (#36). The config
/// `[services.*]` entries are built first, then each ephemeral entry is added. A name is refused
/// at register time if it already exists in config, so a collision here should not occur; if one
/// does, the ephemeral entry wins (last-writer) — but the guard is the register-time check.
pub fn build_services_with_ephemeral(
    cfg: &Config,
    audit: &AuditSink,
    limiters: &Arc<crate::limits::MeshLimiters>,
    ephemeral: &HashMap<String, EphemeralService>,
) -> Services {
    let mut map: HashMap<String, ServiceEntry> = HashMap::new();
    for (name, svc) in &cfg.services {
        let backend = match svc.backend_result() {
            Ok(Backend::Run(cmd)) => Arc::new(session_backend_run(
                cmd,
                &svc.env,
                svc.cwd.as_deref(),
                name,
                cfg,
                audit,
                limiters,
                svc.rate_limit_per_min,
            )) as Arc<dyn SessionBackend>,
            Ok(Backend::Socket(path)) => Arc::new(session_backend_socket(
                path,
                name,
                audit,
                limiters,
                svc.rate_limit_per_min,
            )) as Arc<dyn SessionBackend>,
            Err(e) => {
                tracing::warn!(service = %name, %e, "skipping malformed service");
                continue;
            }
        };
        map.insert(
            name.clone(),
            ServiceEntry {
                backend,
                allow: svc.allow.clone(),
                kind: match svc.backend_result() {
                    Ok(Backend::Socket(_)) => mcpmesh_net::ServiceKind::Socket,
                    _ => mcpmesh_net::ServiceKind::Run,
                },
                ephemeral: false,
            },
        );
    }
    // Overlay ephemeral registrations (in-memory only). BackendSpec is the protocol's own backend
    // shape; map it to the same SpawnBackend/SocketBackend the config path builds.
    for (name, eph) in ephemeral {
        let backend = match &eph.backend {
            mcpmesh_local_api::BackendSpec::Run { cmd, env, cwd } => Arc::new(session_backend_run(
                cmd,
                env,
                cwd.as_deref(),
                name,
                cfg,
                audit,
                limiters,
                eph.rate_limit_per_min,
            ))
                as Arc<dyn SessionBackend>,
            mcpmesh_local_api::BackendSpec::Socket { path } => Arc::new(session_backend_socket(
                path,
                name,
                audit,
                limiters,
                eph.rate_limit_per_min,
            ))
                as Arc<dyn SessionBackend>,
        };
        map.insert(
            name.clone(),
            ServiceEntry {
                backend,
                allow: eph.allow.clone(),
                kind: match &eph.backend {
                    mcpmesh_local_api::BackendSpec::Socket { .. } => {
                        mcpmesh_net::ServiceKind::Socket
                    }
                    mcpmesh_local_api::BackendSpec::Run { .. } => mcpmesh_net::ServiceKind::Run,
                },
                ephemeral: true,
            },
        );
    }
    Services::new(map)
}

#[allow(clippy::too_many_arguments)]
fn session_backend_run(
    cmd: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&str>,
    name: &str,
    cfg: &Config,
    audit: &AuditSink,
    limiters: &Arc<crate::limits::MeshLimiters>,
    // #63: `[services.<name>].rate_limit_per_min`, or an ephemeral registration's clamped value.
    rate: Option<u32>,
) -> SpawnBackend {
    SpawnBackend {
        cmd: cmd.to_vec(),
        env: env.clone(),
        cwd: cwd.map(str::to_string),
        concurrency: Arc::new(Semaphore::new(spawn_concurrency(cfg))),
        service: name.to_string(),
        audit: audit.clone(),
        limiter: limiters.for_service(name, rate),
    }
}

fn session_backend_socket(
    path: &str,
    name: &str,
    audit: &AuditSink,
    limiters: &Arc<crate::limits::MeshLimiters>,
    rate: Option<u32>,
) -> SocketBackend {
    SocketBackend {
        path: path.to_string(),
        service: name.to_string(),
        audit: audit.clone(),
        limiter: limiters.for_service(name, rate),
    }
}

/// A short, human-glanceable fingerprint of an endpoint id: the first 8 chars of its base32
/// (`EndpointId`'s `Display`) form. The default self-nickname when config sets none (
/// "suggested nickname"). Not security-bearing — the id itself is the routing key.
fn short_fingerprint(id: &iroh::EndpointId) -> String {
    id.to_string().chars().take(8).collect()
}

/// A friendly default display name for this node when the config sets no `nickname`: the machine's
/// short hostname, else the endpoint fingerprint. So a freshly-started daemon advertises `jetson`
/// instead of `96246d3f` out of the box (a config `nickname` still wins; a peer's stored nickname is
/// captured at pairing time from whatever the peer suggests here).
fn default_self_nickname(id: &iroh::EndpointId) -> String {
    hostname_nickname().unwrap_or_else(|| short_fingerprint(id))
}

/// This machine's `hostname`, sanitized into a nickname, or `None` if the command fails or is empty.
fn hostname_nickname() -> Option<String> {
    let out = std::process::Command::new("hostname").output().ok()?;
    sanitize_hostname(&String::from_utf8_lossy(&out.stdout))
}

/// Sanitize a raw hostname into a nickname: the short name (before the first `.`), lowercased, keeping
/// only `[a-z0-9-]`; `None` if the result is empty. Pure — the fallible `hostname` call is separate.
fn sanitize_hostname(raw: &str) -> Option<String> {
    let short = raw
        .trim()
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let cleaned: String = short
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Assemble a serving [`DaemonState`] around an already-bound endpoint and peer store, for
/// in-process integration tests that must drive the REAL control server (`serve_control`) —
/// the proxy round-trip test binds a control socket over this and runs `mcpmesh connect` as
/// a subprocess against it, so the actual `open_session` dial-by-id + pipe are exercised. The
/// mesh's serve loop is inert here (`open_session` reads only the endpoint + store to DIAL
/// outbound); production assembles its own `MeshState` inline in `serve_forever`.
pub fn serving_state(endpoint: iroh::Endpoint, store: Arc<PeerStore>) -> Arc<DaemonState> {
    let gate: Arc<dyn TrustGate> = Arc::new(AllowlistGate::new(store.clone()));
    let self_nickname = short_fingerprint(&endpoint.id());
    // No accept loop is spawned here (this seam only dials OUTBOUND via `open_session`), so the
    // mesh's `accept_task` stays empty.
    let mesh = MeshState::new(
        endpoint,
        gate,
        store,
        Arc::new(LiveInvites::new()),
        self_nickname,
        // Test-only dial seam (no roster install runs through it): a HOME-less env
        // degrades to an empty config path rather than failing the seam.
        paths::default_config_path().unwrap_or_default(),
        Arc::new(RosterGate::empty()),
        Arc::new(ConnRegistry::new()),
        None,
        None,
        None,
        None,
    );
    Arc::new(DaemonState::with_mesh(STACK_VERSION, mesh))
}

/// Test-only assembly shared by the daemon submodules' unit tests.
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;
    use std::sync::Arc;

    use mcpmesh_net::TrustGate;
    use mcpmesh_net::registry::ConnRegistry;

    use crate::allowlist::{AllowlistGate, PeerStore};
    use crate::pairing::LiveInvites;
    use crate::roster::gate::{ComposedGate, RosterGate};

    use super::MeshState;
    use super::boot::build_endpoint;

    /// Build a HERMETIC mesh (relay-disabled endpoint, temp config/store, EMPTY roster) so we can
    /// drive `org_join` + `roster_status` in-process against the real config-write + status paths.
    pub(crate) async fn hermetic_mesh(config_path: PathBuf) -> Arc<MeshState> {
        hermetic_mesh_with_invites(config_path, Arc::new(LiveInvites::new())).await
    }

    /// [`hermetic_mesh`] with an explicit invite registry — a file-backed one models what
    /// `boot_node` builds (#87b), so a test can assert the DAEMON path persists rather than only
    /// that `LiveInvites` can.
    pub(crate) async fn hermetic_mesh_with_invites(
        config_path: PathBuf,
        invites: Arc<LiveInvites>,
    ) -> Arc<MeshState> {
        hermetic_mesh_inner(config_path, invites, false).await
    }

    /// [`hermetic_mesh`] whose endpoint carries the #229 hooks, ARMED over this mesh's store and
    /// roster gate and installed on the mesh — as `boot_node` does. The revoke paths' close pass and
    /// the `before_connect` veto only exist on such a mesh, so a test of either needs this one.
    pub(crate) async fn hermetic_hooked_mesh(config_path: PathBuf) -> Arc<MeshState> {
        hermetic_mesh_inner(config_path, Arc::new(LiveInvites::new()), true).await
    }

    async fn hermetic_mesh_inner(
        config_path: PathBuf,
        invites: Arc<LiveInvites>,
        hooked: bool,
    ) -> Arc<MeshState> {
        let dir = config_path.parent().unwrap();
        let store = Arc::new(PeerStore::open(&dir.join("state.redb")).unwrap());
        let pairs = Arc::new(AllowlistGate::new(store.clone()));
        let roster = Arc::new(RosterGate::empty());
        let gate: Arc<dyn TrustGate> = Arc::new(ComposedGate::new(roster.clone(), pairs));
        let hermetic = crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            ..Default::default()
        };
        let hooks = hooked.then(crate::daemon::hooks::MeshHooks::new);
        let endpoint = build_endpoint(
            iroh::SecretKey::from_bytes(&[7u8; 32]),
            &hermetic,
            false,
            hooks.clone(),
        )
        .await
        .unwrap();
        let mesh = MeshState::new(
            endpoint,
            gate,
            store,
            invites,
            "test".into(),
            config_path,
            roster,
            Arc::new(ConnRegistry::new()),
            None,
            None,
            None,
            None,
        );
        if let Some(hooks) = hooks {
            let _armed = hooks.arm(crate::daemon::hooks::DialGate::new(
                mesh.store.clone(),
                mesh.roster.clone(),
            ));
            mesh.set_peer_hooks(hooks);
        }
        // Model a BOOTED daemon: `MeshState::new` installs an EMPTY registry, and boot then swaps
        // in the built services before the control socket exists. #100 made that load-bearing —
        // `status` and `peer_services` now answer from the registry, so a harness that left it
        // empty was asserting against a state no live daemon is ever observable in.
        let cfg = crate::config::Config::load(&mesh.config_path).unwrap_or_default();
        let ephemeral = mesh
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned")
            .clone();
        crate::daemon::accept::swap_services(
            &mesh,
            crate::daemon::build_services_with_ephemeral(
                &cfg,
                &mesh.audit(),
                &mesh.limits(),
                &ephemeral,
            ),
        );
        mesh
    }
}

#[cfg(test)]
mod alpn_rebind_tests {
    use crate::daemon::testutil::hermetic_mesh;

    /// #67: re-advertising after a registration must never NARROW the built-in ALPN set.
    ///
    /// The first `rebind_alpns` recomputed the built-ins from `alpns_for(roster_transport_live())`
    /// — `gossip.is_some()` — while boot binds `alpns_for(org_root_pk.is_some())`. Those diverge
    /// exactly where this file documents (roster mode on, no `org_id` resolvable), and a
    /// registration there silently dropped the gossip and roster-blob ALPNs the endpoint had been
    /// serving. Measured, in review.
    ///
    /// Asserted against the recorded set rather than through a live handshake, because the
    /// divergent state needs a node booted with a pinned org root and no resolvable org_id — and
    /// the property is about which bytes get re-advertised, which is exactly what this compares.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_registration_never_narrows_the_bound_alpn_set() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = hermetic_mesh(cfg).await;

        // Model the divergent case directly: boot recorded the ROSTER set (as it would with an org
        // root pinned), while this mesh composes no gossip — so `roster_transport_live()` is false
        // and the old recomputation would have produced the smaller pairing set.
        let bound = crate::daemon::boot::alpns_for(true);
        mesh.set_bound_alpns(bound.clone());
        assert!(
            !mesh.roster_transport_live(),
            "precondition: the two signals disagree here, which is the whole case"
        );

        mesh.register_app_protocol(b"app/x/1", std::sync::Arc::new(Nothing))
            .expect("an app ALPN registers");

        // Every ALPN the endpoint was bound with must survive the re-advertise.
        let after = mesh.advertised_alpns_for_test();
        for a in &bound {
            assert!(
                after.contains(a),
                "re-advertising dropped a built-in ALPN the endpoint was serving: {}",
                String::from_utf8_lossy(a)
            );
        }
        assert!(
            after.iter().any(|a| a == b"app/x/1"),
            "…and the registered one must be added"
        );
    }

    #[derive(Debug)]
    struct Nothing;

    impl iroh::protocol::ProtocolHandler for Nothing {
        async fn accept(
            &self,
            _conn: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {

    /// #63 gate: pin the Arc the backend actually HOLDS, not a side effect of building it.
    ///
    /// The first version asserted on `MeshLimiters::tracked_rpm`, which proves `for_service` was
    /// CALLED with the right arguments — and nothing about the returned limiter being installed.
    /// `{ let _ = limiters.for_service(name, rate); limiters.requests.clone() }` passed the whole
    /// workspace with every backend back on one shared bucket, i.e. with the entire bug restored.
    /// A side-effect assertion is not a call-site assertion.
    #[test]
    fn each_backend_holds_its_own_service_limiter() {
        let cfg = crate::config::Config::from_toml_str(
            "[limits]\nrate_limit_per_min = 50\n\
             [services.noisy]\nsocket = \"/run/a.sock\"\nallow = []\nrate_limit_per_min = 2\n\
             [services.quiet]\nrun = [\"true\"]\nallow = []\n",
        )
        .unwrap();
        let limiters = crate::limits::MeshLimiters::from_config(&cfg.limits);
        let audit = crate::audit::AuditSink::disabled();

        let noisy = session_backend_socket("/run/a.sock", "noisy", &audit, &limiters, Some(2));
        let quiet = session_backend_run(
            &["true".to_string()],
            &Default::default(),
            None,
            "quiet",
            &cfg,
            &audit,
            &limiters,
            None,
        );

        // The INSTALLED limiter must be the service's own, by pointer.
        assert!(
            Arc::ptr_eq(&noisy.limiter, &limiters.for_service("noisy", Some(2))),
            "the socket backend must hold the limiter `for_service` returned for ITS name"
        );
        assert!(
            Arc::ptr_eq(&quiet.limiter, &limiters.for_service("quiet", None)),
            "and so must the run backend"
        );
        assert!(
            !Arc::ptr_eq(&noisy.limiter, &quiet.limiter),
            "two services must NOT share one bucket — a shared bucket is the whole of #63"
        );
        assert!(
            !Arc::ptr_eq(&noisy.limiter, &limiters.requests),
            "and neither may be the old shared `requests` limiter"
        );

        // …and behaviourally: draining the noisy service leaves the quiet one untouched.
        let eid = mcpmesh_net::EndpointId::from_bytes([3u8; 32]);
        let t = std::time::Instant::now();
        assert!(noisy.limiter.check(&eid, t).is_ok());
        assert!(noisy.limiter.check(&eid, t).is_ok());
        assert!(noisy.limiter.check(&eid, t).is_err(), "noisy is drained");
        assert!(
            quiet.limiter.check(&eid, t).is_ok(),
            "the quiet service must still admit"
        );
    }

    /// #63: `build_services` must reach the same seam for every configured service.
    #[test]
    fn build_services_gives_each_service_its_own_bucket() {
        let cfg = crate::config::Config::from_toml_str(
            "[limits]\nrate_limit_per_min = 50\n\
             [services.noisy]\nsocket = \"/run/a.sock\"\nallow = []\nrate_limit_per_min = 2\n\
             [services.quiet]\nsocket = \"/run/b.sock\"\nallow = []\n",
        )
        .unwrap();
        let limiters = crate::limits::MeshLimiters::from_config(&cfg.limits);
        let _ = build_services_audited(&cfg, &crate::audit::AuditSink::disabled(), &limiters);

        assert_eq!(
            limiters.tracked_rpm("noisy"),
            Some(Some(2)),
            "the per-service rate must reach the BACKEND's limiter, not just parse — a backend \
             still holding the shared `requests` limiter tracks nothing here"
        );
        assert_eq!(
            limiters.tracked_rpm("quiet"),
            Some(Some(50)),
            "a service with no override gets its OWN bucket at the global rate — sharing one \
             bucket is what let the noisy service starve it"
        );
    }
    use super::*;

    #[tokio::test]
    async fn recent_pairings_surfaces_the_inviter_side_sas() {
        // #35: the inviter learns its side of the SAS from status.recent_pairings — the
        // `record_pairing` sink the accept-loop calls on a completed redemption. Newest-first,
        // carrying the SAS words for the out-of-band human check, so an embedder renders the
        // whole ceremony without shelling out to `mcpmesh status`.
        let dir = tempfile::tempdir().unwrap();
        let mesh = super::testutil::hermetic_mesh(dir.path().join("config.toml")).await;
        assert!(mesh.recent_pairings().is_empty(), "no pairings yet");

        mesh.record_pairing(mcpmesh_local_api::RecentPairing {
            peer_nickname: "bob".into(),
            sas_code: "tango-fig-cabbage".into(),
            paired_at_epoch: 1000,
            self_enroll: false,
        });
        mesh.record_pairing(mcpmesh_local_api::RecentPairing {
            peer_nickname: "carol".into(),
            sas_code: "delta-hop-iron".into(),
            paired_at_epoch: 2000,
            self_enroll: false,
        });

        let recent = mesh.recent_pairings();
        assert_eq!(recent.len(), 2);
        // Newest first, each carrying the SAS the inviter reads aloud.
        assert_eq!(recent[0].peer_nickname, "carol");
        assert_eq!(recent[0].sas_code, "delta-hop-iron");
        assert_eq!(recent[1].peer_nickname, "bob");
        assert_eq!(recent[1].sas_code, "tango-fig-cabbage");
    }

    #[test]
    fn sanitize_hostname_makes_a_friendly_nickname() {
        assert_eq!(sanitize_hostname("jetson\n").as_deref(), Some("jetson"));
        assert_eq!(
            sanitize_hostname("Johns-MacBook-Pro.local").as_deref(),
            Some("johns-macbook-pro"),
            "strip the domain, lowercase, keep dashes"
        );
        assert_eq!(
            sanitize_hostname("nvidia jetson!").as_deref(),
            Some("nvidiajetson"),
            "drop spaces + punctuation"
        );
        assert_eq!(sanitize_hostname("   ").as_deref(), None);
        assert_eq!(sanitize_hostname("").as_deref(), None);
        assert_eq!(sanitize_hostname(".local").as_deref(), None);
    }

    #[test]
    fn spawn_concurrency_reads_max_sessions_with_a_safe_floor() {
        let c = Config::from_toml_str("[limits]\nmax_sessions = 2\n").unwrap();
        assert_eq!(super::spawn_concurrency(&c), 2);
        let dflt = Config::from_toml_str("").unwrap();
        assert_eq!(super::spawn_concurrency(&dflt), 4, "default max_sessions");
        let zero = Config::from_toml_str("[limits]\nmax_sessions = 0\n").unwrap();
        assert_eq!(
            super::spawn_concurrency(&zero),
            1,
            "a 0 misconfig floors to 1, never no-permits"
        );
        // Keep the documented default constant PINNED to the config default (and referenced, so it is
        // not dead code once `build_services_audited` switches to `spawn_concurrency(cfg)`).
        assert_eq!(
            super::SPAWN_CONCURRENCY as u32,
            crate::config::LimitsCfg::default().max_sessions,
            "the documented default matches the config default"
        );
    }
}
