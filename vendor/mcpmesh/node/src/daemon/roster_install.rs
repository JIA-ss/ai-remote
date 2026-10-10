//! The roster install pipeline and its enforcement: the swap-before-sever choke point ALL
//! three convergence channels funnel through (the manual `roster_install` verb, gossip, the
//! URL poll — the automatic two reach it via [`converge_roster_bytes`], surfaced to
//! `roster::distribute` as `DistributionHost::install_roster_bytes`), the freshness/staleness
//! sweep, the org join/URL-pin verbs, the per-node roster/sidecar paths, and the tracked
//! poll-loop respawn.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use mcpmesh_local_api::{OrgJoinResult, RosterInstallResult};
use mcpmesh_trust::roster::validate::RosterState;
use tokio::task::JoinHandle;

use crate::audit::{AuditRecord, now_ts};
use crate::config::Config;
use crate::control::DaemonState;
use crate::roster::RosterStore;
use crate::roster::gate::RosterGate;
use crate::util::{TempPathGuard, blocking, epoch_now_i64, unique_temp_path};

use super::MeshState;
use super::config_write::{
    write_identity_nickname, write_identity_pin, write_identity_user_id, write_join_pin,
    write_roster_url,
};

/// Warn — once, at install/load time — when the just-installed roster is serving in DEGRADED-GRACE
/// (past `expires_at` but within the grace window), so serving continues with reduced
/// confidence. Rate-limited to once per swap/load by construction: it
/// fires only from the two install/load sites ([`install_roster_view_and_sever`] + the startup load),
/// never per session. DegradedStopped is intentionally NOT warned here — the gate already refuses
/// roster identity outright (a hard stop the operator sees as refusals), whereas DegradedGrace is the
/// silent-degradation case this warning exists to surface. Reads the state against the gate's OWN
/// grace window so the warning and the `resolve` path agree on when a roster is degraded.
pub(crate) fn warn_if_degraded_grace(roster: &RosterGate) {
    // Route through the gate's EFFECTIVE state (expiry OR staleness) so a roster that is
    // degraded-grace because it is STALE (not just expired) is warned identically. DegradedStopped is
    // intentionally NOT warned here — the gate already refuses roster identity outright.
    if roster.effective_state(epoch_now_i64()) == Some(RosterState::DegradedGrace) {
        tracing::warn!(
            "roster degraded (expired or stale); serving continues for the grace period — \
             re-confirm currency or install a fresh roster"
        );
    }
}

/// Hot-swap the installed roster view into the live gate AND sever the live mesh connections the
/// new roster invalidates. Delegates the per-connection decision to the pure
/// [`mcpmesh_net::should_sever`]: an endpoint is severed iff it is REVOKED (revocation wins, all
/// ALPNs — this cuts a device even when it also holds a stale pair entry), OR it was
/// ROSTER-resolved (`roster_user.is_some()`) and is ABSENT from the new roster's active device
/// set. A pairing-only peer (`roster_user == None`, not revoked) is NEVER severed by a roster
/// install.
///
/// **Ordering — swap-before-sever (the TOCTOU close).** The sever sets are computed from the
/// NEW `view` handle DIRECTLY (never by locking the gate), then the gate view is hot-swapped FIRST,
/// THEN `sever_matching` runs. Swapping first means (a) no NEW session admits the revoked peer, and
/// (b) any connection CHECK-REGISTERING concurrently reads the new view and self-closes; a
/// registration that lands across the swap is caught by the registry's lock-serialized recheck (see
/// the `registry` module doc's three-case argument). Computing the sets from `view` — not the gate —
/// keeps the registry lock in `sever_matching` from ever nesting inside the gate's `RwLock`, so
/// there is no registry↔gate lock cycle. Returns the number of connections severed.
///
/// `pub` so the sever integration tests (`cli/tests/roster_sever.rs` and friends) drive the SAME
/// persist→swap→sever pipeline the install control uses.
pub fn install_roster_view_and_sever(
    mesh: &MeshState,
    view: mcpmesh_trust::roster::validate::RosterView,
) -> usize {
    use std::collections::HashSet;
    // Capture the audit fields BEFORE the swap CONSUMES `view` (surface-clean target: org/serial
    // only). This is the SINGLE choke point ALL three convergence channels funnel through (manual
    // install_roster + gossip on_announce + URL poll), and it is reached ONLY on a real serial-bumping
    // install — `install_from_file` (and the manual path) return a view ONLY when serial > installed;
    // a stale/equal serial errors BEFORE any swap — so the trust event below fires EXACTLY once per
    // real swap (incl. a joiner's first serial-1 install, a legit trust event).
    let (org_id, serial) = (view.org_id().to_string(), view.serial());
    // Compute the sever sets from the NEW view (no gate lock → no registry↔gate lock cycle).
    // The trust view speaks raw 32-byte ids; net's sever rule speaks EndpointId — convert once here.
    let revoked: HashSet<mcpmesh_net::EndpointId> =
        view.revoked_endpoints().map(|b| (*b).into()).collect();
    let active_devices: HashSet<mcpmesh_net::EndpointId> =
        view.device_endpoints().map(|b| (*b).into()).collect();
    // (a) SWAP FIRST: hot-swap the RosterGate view so the composed gate + any concurrent
    //     check-register immediately see the new roster (the other half of the TOCTOU invariant).
    mesh.roster.install(view);
    // Warn (once, on this swap) if the newly-installed roster is already in degraded-grace:
    // serving continues within the grace window, but the operator should install a fresh one.
    warn_if_degraded_grace(&mesh.roster);
    // (b) THEN sever the already-registered live connections the new roster invalidates.
    let severed = mesh.conn_registry.sever_matching(
        mcpmesh_net::CLOSE_UNAUTHORIZED, // 401 — "no longer authorized"
        b"roster revoked",
        |eid, roster_user| mcpmesh_net::should_sever(eid, roster_user, &revoked, &active_devices),
    );
    // (c) #229: close the connections this node DIALLED (the hook registry is outbound-only) to devices
    //     the new roster makes refused. Async (the refusal reads are redb, on the blocking pool)
    //     and this function is sync, so it is spawned; the gate is already swapped, so a dial racing
    //     it is refused at `before_connect` or at its `after_handshake` re-check. Every production
    //     caller runs inside the runtime; a caller outside one gets a warning, not a silent skip.
    if let Some(h) = mesh.peer_hooks.get().cloned()
        && let Some(gate) = h.gate()
    {
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    let closed = super::hooks::close_refused(&h.conns(), &gate).await;
                    if closed > 0 {
                        tracing::info!(
                            closed,
                            "roster install closed connections to now-refused devices"
                        );
                    }
                });
            }
            Err(_) => tracing::warn!(
                "roster installed outside a tokio runtime: connections this node opened to devices \
                 it revoked were NOT closed"
            ),
        }
    }
    // Trust event: a roster install/swap — recorded HERE, the shared choke point, so EVERY swap
    // is audited, including the AUTOMATIC gossip/URL convergences a rostered node lives on
    // (which never touch the manual verb). The terminus of org approve/revoke funnels through here
    // too, so the manual path stays audited — once. Surface-clean: org/serial only, NO keys/EndpointIds.
    mesh.audit().record(AuditRecord::trust(
        now_ts(),
        "roster_install".into(),
        Some(format!("{org_id}/{serial}")),
        // #57: deliberately NO principal — a roster install is a purely local act (manual verb
        // or automatic convergence), not attributable to any remote subject.
        None,
    ));
    severed
}

/// Persist an adopted successor org root (#93 ask c), audibly.
///
/// Called from EVERY install path — the manual one and the gossip/URL convergence one — because a
/// node that was offline across a rotation catches up through the second, so re-anchoring only in
/// the first would fix the case that needs it least. Extracted rather than duplicated for exactly
/// that reason.
///
/// Durable: a rotation held only in memory reverts on restart and re-strands the node, which is the
/// failure this feature exists to remove. Written through the same surgical RMW writer, under the
/// `reload_lock` the caller already holds — a nested lock on the non-reentrant tokio mutex would
/// deadlock.
///
/// An org's trust anchor moving is the highest-consequence event in roster mode and it happens with
/// NO operator action on this node, so it is audited and logged at `warn!`.
async fn adopt_successor_root(
    mesh: &MeshState,
    anchor: &mcpmesh_trust::roster::validate::AnchorChange,
    org_id: &str,
) -> anyhow::Result<()> {
    let Some(new_pk) = anchor.adopted_root_pk.clone() else {
        return Ok(());
    };
    let config_path = mesh.config_path.clone();
    let (pk_w, oid_w) = (new_pk.clone(), org_id.to_string());
    blocking("join org-root rotation config write", move || {
        write_identity_pin(&config_path, &pk_w, &oid_w)
    })
    .await??;
    let _ = new_pk;
    audit_successor_adoption(mesh, anchor, org_id);
    Ok(())
}

/// The audit record + warning for an adopted successor, split from the WRITE so the manual install
/// path can settle pin precedence against an explicit caller pk itself and still report exactly
/// once.
fn audit_successor_adoption(
    mesh: &MeshState,
    anchor: &mcpmesh_trust::roster::validate::AnchorChange,
    org_id: &str,
) {
    let Some(new_pk) = anchor.adopted_root_pk.as_deref() else {
        return;
    };
    mesh.audit().record(crate::audit::AuditRecord::trust(
        crate::audit::now_ts(),
        "org_root_rotated".into(),
        Some(org_id.to_string()),
        Some(new_pk.to_string()),
    ));
    tracing::warn!(
        org_id = %org_id,
        new_root = %new_pk,
        "ORG ROOT ROTATED — this node re-anchored to the successor named by the previous root"
    );
}

/// Converge fetched roster BYTES through the SINGLE install path — the shared convergence tail of
/// BOTH automatic distribution channels (the gossip announce and the URL poll), surfaced to
/// `roster::distribute` as `DistributionHost::install_roster_bytes`: serialized under
/// `mesh.reload_lock` (the SAME single-writer discipline as the manual install), re-check
/// `serial > installed` INSIDE the lock (a racing channel may have installed it — idempotent
/// no-op), resolve the pinned org-root anchor, bridge the bytes to a temp file, and converge via
/// `RosterStore::install_from_file` — the ONLY validator (including the org-root SIGNATURE +
/// serial>installed; the announce/blob/hash/served-body only TRIGGERED the fetch) — then confirm
/// freshness (a validated roster from an authenticated channel is proof of currency),
/// hot-swap + sever ([`install_roster_view_and_sever`]), and reconcile the config user_id.
///
/// Returns whether an install actually happened (`false` = lost the under-lock serial race — a
/// fail-safe no-op). The CALLER re-seeds/re-announces on `true` (the channels differ in how they
/// treat an announce failure). The MANUAL [`install_roster`] deliberately does NOT route through
/// here: it takes a PATH (not bytes), resolves/pins an EXPLICIT trust anchor under its own held
/// `reload_lock`, and returns the severed count to the operator — a different contract.
pub(crate) async fn converge_roster_bytes(
    mesh: &MeshState,
    bytes: &[u8],
    serial: u64,
    channel: &'static str,
) -> Result<bool> {
    let _reload = mesh.reload_lock.lock().await;
    if serial <= mesh.roster.view().map(|v| v.serial()).unwrap_or(0) {
        return Ok(false);
    }
    let pk = crate::roster::parse_org_root_pk(
        &mesh_config_org_root_pk(mesh)?.context("no pinned org root; cannot accept roster")?,
    )?;
    let tmp = write_temp_roster(&roster_staging_dir(mesh), bytes)?;
    let rstore = RosterStore::new(installed_roster_path(mesh));
    let now = epoch_now_i64();
    // `tmp` moves into the closure: its guard removes the temp file when the install returns
    // (success and failure alike).
    let (view, anchor) = blocking("join roster install", move || {
        rstore.install_from_file(tmp.path(), &pk, now)
    })
    .await??;
    // #93 ask c: the convergence path is how a node that was OFFLINE across a rotation catches up,
    // so it needs the re-anchor at least as much as the manual one.
    adopt_successor_root(mesh, &anchor, view.org_id()).await?;
    mesh.confirm_roster_current(now).await;
    let severed = install_roster_view_and_sever(mesh, view);
    reconcile_user_id_from_roster(mesh).await;
    drop(_reload);
    tracing::info!(serial, severed, channel, "installed roster");
    Ok(true)
}

/// How often the staleness sweep runs. 60 s mirrors the revocation-propagation target —
/// an existing roster-authorized session is cut within a minute of the node crossing the
/// staleness bound.
const STALENESS_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// The sweep DECISION (pure): sweep iff the gate is effectively `DegradedStopped` —
/// expiry OR staleness past grace. `None` (no roster) → never sweep. `Approved`/`DegradedGrace`
/// keep serving. This is the time-triggered complement to the register-time gate.
/// `pub` (not `pub(crate)`) so the `staleness_sweep.rs` integration test — a separate crate — drives it.
pub fn should_staleness_sever(state: Option<mcpmesh_trust::roster::validate::RosterState>) -> bool {
    state == Some(mcpmesh_trust::roster::validate::RosterState::DegradedStopped)
}

/// One sweep tick: when the gate is `DegradedStopped`, sever EXISTING roster-
/// authorized live sessions (`roster_user.is_some()`) — the time-triggered cut the register-time
/// `should_sever_now` cannot make. NEVER severs pairing-only sessions (`roster_user == None`, whose
/// authorization is independent of the org roster) and never runs when the roster is fresh/absent.
/// Returns the number severed. `pub` so the `staleness_sweep.rs` integration test (a separate crate)
/// drives one tick directly.
pub fn staleness_sweep_once(mesh: &MeshState, now: i64) -> usize {
    if !should_staleness_sever(mesh.roster.effective_state(now)) {
        return 0;
    }
    mesh.conn_registry.sever_matching(
        mcpmesh_net::CLOSE_UNAUTHORIZED, // 401 — the roster no longer authorizes this session
        b"roster stale",
        |_eid, roster_user| roster_user.is_some(),
    )
}

/// Spawn the periodic staleness sweep (roster mode). Runs for the daemon lifetime; each tick calls
/// [`staleness_sweep_once`] at the current clock.
pub(crate) fn spawn_staleness_sweep(mesh: Arc<MeshState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(STALENESS_SWEEP_INTERVAL).await;
            let severed = staleness_sweep_once(&mesh, epoch_now_i64());
            if severed > 0 {
                tracing::warn!(
                    severed,
                    "staleness sweep cut roster-authorized sessions (degraded-stopped)"
                );
            }
        }
    })
}

/// Handle a `roster_install` control request: resolve the org-root trust
/// anchor, read + FULLY validate the roster file (rules 1–6), persist it atomically, hot-swap the
/// gate, and sever the live sessions it invalidates (via [`install_roster_view_and_sever`]).
///
/// **Pin flow (DECLARED — pin AFTER validation).** An explicit `org_root_pk` is the trust anchor for
/// a FIRST install (or an operator re-pin); once pinned it lives in config, so a subsequent install
/// OMITS it and reads the pinned value. The pin is persisted (`[identity] org_root_pk` + `org_id`)
/// only AFTER `install_from_file` accepts the roster — so a failed install (wrong pk, rollback, an
/// unparseable file) NEVER corrupts the pinned anchor or the on-disk roster. This is a deliberate
/// improvement over the plan's pin-before-validate sketch: the plan's `pin_org_root_pk(mesh, &s)`
/// had no `org_id` to write at pin time (org_id is only known after validation), and re-pinning
/// before a validation that then fails would leave the anchor pointing at a pk the installed roster
/// no longer verifies against. A mismatched pk simply fails validation (the sig won't verify) and
/// leaves everything untouched.
///
/// Path-not-bytes: the same-uid daemon reads the LOCAL file itself — passing a path, not
/// the bytes, is within the trust boundary (the operator obtains the roster + the pk
/// out-of-band).
pub(crate) async fn install_roster(
    state: &DaemonState,
    path: String,
    org_root_pk: Option<String>,
) -> Result<RosterInstallResult> {
    let mesh = state.mesh_required()?;
    // Serialize the ENTIRE install critical section — installed-serial read → validate → persist →
    // pin → gate hot-swap → sever — under the SAME `reload_lock` that register_service /
    // grant_service_access / revoke_service_access hold around their whole read→write→swap sections
    // (the shared single-writer discipline). Without it, two concurrent same-uid `roster_install`s could BOTH read
    // installed_serial = N, BOTH validate N+1 / N+2, and race their writes+swaps → roster.json + the
    // live gate left at the LOWER serial (a non-monotonic lost update). Held until this fn returns.
    //
    // Lock ordering stays acyclic: `reload_lock` is only ever a SOURCE (nothing acquires it while
    // holding another lock). Under it we touch the config write (spawn_blocking), the RosterGate
    // RwLock (`gate.install`, released before the sever), and the ConnRegistry mutex
    // (`sever_matching`, whose sets come from the VIEW — not by reading the gate). `register_checked`
    // takes registry→gate but NEVER `reload_lock`, so there is no back-edge and no cycle. Holding it
    // across the spawn_blocking / sever awaits is fine (tokio Mutex, exactly as register_service).
    let _reload = mesh.reload_lock.lock().await;

    // Resolve the trust anchor: an explicit `org_root_pk` (first install / re-pin), else the pinned
    // config value; error if neither. The pin itself is written AFTER validation succeeds (below).
    let pk_str = match &org_root_pk {
        Some(s) => s.clone(),
        None => mesh_config_org_root_pk(mesh)?
            .context("no org root pinned; pass --org-root-pk on first install")?,
    };
    let pk = crate::roster::parse_org_root_pk(&pk_str)?;
    // Per-node like every other roster location: derived from `mesh.config_path` (see
    // `installed_roster_path`), production-identical to the old global default_roster_path.
    let rstore = RosterStore::new(installed_roster_path(mesh));
    let now = epoch_now_i64();
    let file = PathBuf::from(path);
    // Read + validate + persist on a blocking thread (fs + verify); returns the resolvable view.
    // A FAILED validation returns Err BEFORE the write, so the on-disk roster is left untouched.
    let (view, anchor) = blocking("join roster install", move || {
        rstore.install_from_file(&file, &pk, now)
    })
    .await??;
    let (org_id, serial) = (view.org_id().to_string(), view.serial());
    // Audit + log the adoption; the PIN is written by the block below, which also settles the
    // precedence against an explicit caller pk.
    audit_successor_adoption(mesh, &anchor, &org_id);
    // Pin the trust anchor + org_id to config now that the roster validated — only when an explicit
    // pk was provided (a first install or an operator re-pin). Call the lock-free `write_identity_pin`
    // DIRECTLY under the `reload_lock` we ALREADY hold — a nested `reload_lock.lock()` on the
    // non-reentrant tokio Mutex would deadlock. Same order as before: pin AFTER validation, using the
    // validated org_id; a subsequent install that reused the already-pinned value does not rewrite it.
    // An ADOPTED successor wins over the caller's explicit pk (#93 ask c gate).
    //
    // The caller's pk is by construction the OLD anchor — it is what they had to supply to make the
    // roster verify at all. Pinning it after adoption silently reverted the re-anchor while the
    // `org_root_rotated` audit record and warning had already fired: "reads as durable, then
    // silently reverts", the exact failure `AnchorChange`'s own doc cites #107 for. Reachable by
    // the by-hand distribution the CLI recommends, and by any joiner holding only the `org_create`
    // invite.
    let pk_str = anchor.adopted_root_pk.clone().unwrap_or(pk_str);
    if org_root_pk.is_some() || anchor.adopted_root_pk.is_some() {
        let config_path = mesh.config_path.clone();
        let (pk_w, oid_w) = (pk_str.clone(), org_id.clone());
        blocking("join org-root pin config write", move || {
            write_identity_pin(&config_path, &pk_w, &oid_w)
        })
        .await??;
        tracing::info!(org_id = %org_id, "pinned org root");
    }
    // Freshness: a manual install is proof of currency — CONFIRM so the live gate's future
    // resolve/should_sever_now decisions treat this node as fresh (arms `last_confirmed` + persists).
    mesh.confirm_roster_current(now).await;
    // Hot-swap the live gate + sever. Runs on the runtime (no blocking; close is non-blocking).
    let severed = install_roster_view_and_sever(mesh, view) as u32;
    // Reconcile config's proposed user_id against the roster's AUTHORITATIVE value —
    // the SAME shared post-install step the gossip/URL channels run. LOCK-FREE under the
    // `reload_lock` we ALREADY hold (no deadlock — see `reconcile_user_id_from_roster`).
    reconcile_user_id_from_roster(mesh).await;
    tracing::info!(org_id = %org_id, serial, severed, "roster installed");
    // NOTE: the `roster_install` trust event is recorded INSIDE
    // `install_roster_view_and_sever` (the shared choke point called above), so the manual path is
    // audited there — once — alongside the gossip/URL convergence paths. No record here.
    // Announce-on-publish: the operator's serial bump (org approve/revoke terminate here)
    // seeds the new roster into the local blob store + broadcasts a `RosterAnnounce` on the roster
    // topic, so rostered peers converge without waiting for their URL poll. Best-effort — a gossip
    // failure must NOT fail the install (the roster is already persisted + hot-swapped); a
    // pure-pairing daemon (no gossip) no-ops. Runs under the held `reload_lock` (announce does not
    // take it, so no deadlock).
    if let Err(e) = crate::roster::distribute::announce_roster(mesh).await {
        tracing::warn!(%e, "roster announce-on-publish failed (install still applied)");
    }
    Ok(RosterInstallResult {
        org_id,
        serial,
        severed,
    })
}

/// Read the currently-pinned org-root pubkey (`[identity] org_root_pk`) from config, or `None` when
/// none is pinned. A plain read (no `reload_lock`): `atomic_write` publishes config by rename, so a
/// read can never observe a torn file. Used by [`install_roster`] to resolve the anchor on a
/// subsequent install that OMITS `--org-root-pk`.
pub(crate) fn mesh_config_org_root_pk(mesh: &MeshState) -> Result<Option<String>> {
    let cfg = Config::load(&mesh.config_path)
        .map_err(|e| anyhow::anyhow!("read config for org_root_pk: {e}"))?;
    Ok(cfg.identity.org_root_pk)
}

/// The installed-roster path for THIS daemon, derived from `mesh.config_path` (its parent + the
/// canonical `roster.json` name). Production-identical to [`paths::default_roster_path`] — the daemon
/// always runs with `config_path = config_dir()/config.toml`, so the parent is `config_dir()` and
/// this equals `config_dir()/roster.json`. Deriving it from `config_path` (rather than the global
/// path) keeps the gossip/URL install paths per-node in the in-process multi-node integration tests,
/// where two daemons in ONE process must NOT share a single global `roster.json` — and the config
/// read that resolves the trust anchor (`mesh_config_org_root_pk`) already uses the same per-node
/// `config_path`, so the anchor and the roster file always co-locate. **DECLARED** as the one
/// deviation from the plan's literal `paths::default_roster_path()` in `distribute.rs`.
/// Where a roster is STAGED before the install judges it — the installed roster's own directory,
/// derived from `config_path` exactly as [`installed_roster_path`] derives `roster.json`.
pub(crate) fn roster_staging_dir(mesh: &MeshState) -> PathBuf {
    mesh.config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn installed_roster_path(mesh: &MeshState) -> PathBuf {
    mesh.config_path
        .parent()
        .map(|dir| dir.join("roster.json"))
        .unwrap_or_else(|| PathBuf::from("roster.json")) // parentless config_path: never in practice
}

/// The per-node freshness sidecar path, derived from `config_path` (sibling
/// `roster.confirmed`) exactly as [`installed_roster_path`] derives `roster.json` — so the sidecar
/// co-locates with the installed roster and stays per-node in the in-process multi-daemon integration
/// tests. Production-identical to [`paths::default_roster_confirmed_path`] (the daemon runs with
/// `config_path = config_dir()/config.toml`). Takes `&Path` (not `&Arc<MeshState>`) so the `&self`
/// [`MeshState::confirm_roster_current`] and the startup bootstrap can both call it.
pub(crate) fn roster_confirmed_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|dir| dir.join("roster.confirmed"))
        .unwrap_or_else(|| PathBuf::from("roster.confirmed")) // parentless: never in practice
}

/// Reconcile the daemon's config `[identity].user_id` against the just-installed roster
/// **The roster is AUTHORITATIVE for a device's user_id:** config's
/// `[identity].user_id` was a best-effort join-time PROPOSAL, and the roster's assignment for THIS
/// device's endpoint WINS. Resolve our own endpoint id in `mesh.roster.view()`; if the roster
/// assigns it a user_id that DIFFERS from config's, surgically rewrite config to the roster value —
/// NEVER the reverse (config never overwrites the roster). Two no-op cases: the roster resolves our
/// endpoint to the SAME user_id (nothing to change), OR this device is ABSENT from the roster (the
/// view is `None`, or `resolve` returns `None`) — config is left untouched (still pending), no write.
///
/// **Own endpoint id.** `mesh.endpoint.id()` is the ed25519 pubkey the endpoint is seeded from — the
/// SAME value a roster device record names (the daemon's own `our_id`; reused rather than recomputed
/// from the device key). We look it up directly in the installed view.
///
/// **Locking (DECLARED — LOCK-FREE under the held `reload_lock`).** Every caller runs this UNDER the
/// `mesh.reload_lock` it already holds — the manual [`install_roster`] and the gossip
/// [`on_announce`](crate::roster::distribute) / URL [`poll_roster_url_once`](crate::roster::distribute)
/// install paths — the SAME single-writer critical section the install itself uses. So it calls the
/// LOCK-FREE [`write_identity_user_id`] DIRECTLY (a nested `reload_lock.lock()` on the non-reentrant
/// tokio Mutex would DEADLOCK), EXACTLY as `install_roster` calls the lock-free `write_identity_pin`
/// under the same held lock. The config write runs on a blocking worker (the fs house rule).
///
/// Runs on EVERY roster install (all three channels share this post-install helper), so a roster
/// arriving by ANY channel reconciles + (via the live `roster_status`) flips pending → approved.
pub(crate) async fn reconcile_user_id_from_roster(mesh: &MeshState) {
    // Resolve OUR own endpoint in the just-installed view. Absent (view None, or not in the roster) →
    // leave config untouched (still pending): no write.
    let our_id = mesh.endpoint.id();
    let Some(roster_user_id) = mesh
        .roster
        .view()
        .and_then(|v| v.resolve(our_id.as_bytes()).map(|d| d.user_id.clone()))
    else {
        return;
    };
    // The roster is authoritative: only WRITE when its value DIFFERS from config's proposal (a match
    // — or an unreadable config, treated as no proposal to overwrite — is a no-op).
    let config_path = mesh.config_path.clone();
    let proposed = Config::load(&config_path)
        .ok()
        .and_then(|c| c.identity.user_id);
    if proposed.as_deref() == Some(roster_user_id.as_str()) {
        return;
    }
    // Rewrite config to the ROSTER's authoritative value. LOCK-FREE writer under the caller's HELD
    // `reload_lock` (see the doc note — a nested lock would deadlock); on a blocking worker.
    let uid = roster_user_id.clone();
    match blocking("join reconcile config user_id write", move || {
        write_identity_user_id(&config_path, &uid)
    })
    .await
    {
        Ok(Ok(())) => {
            tracing::info!(user_id = %roster_user_id, "reconciled config user_id from the authoritative roster")
        }
        Ok(Err(e)) | Err(e) => tracing::warn!(%e, "reconcile config user_id write failed"),
    }
}

/// Write roster BYTES to a unique temp file so [`RosterStore::install_from_file`] — the SINGLE
/// convergence path (validate rules 1–6 → persist) — can judge them. The gossip/URL channels fetch
/// bytes, but the install path takes a PATH (the same-uid daemon reads its own local file);
/// this bridges the two. The returned [`TempPathGuard`] removes the temp file on drop — the caller
/// holds it across the install. A per-call-unique name ([`unique_temp_path`]: pid + seq — never
/// a fixed temp name); the file is only ever READ by `install_from_file`.
///
/// Staged BESIDE the installed roster (`mesh.config_path`'s directory), not in the system temp
/// dir. It is per-node state — same argument as [`installed_roster_path`] — so it belongs under the
/// node's own root, where a `--profile` root or an embedded node keeps it separate. The shared temp
/// dir also made it impossible to tell one node's in-flight staging file from another's, which is
/// the difference between "this node leaked a temp" and "some other process is mid-install".
pub(crate) fn write_temp_roster(dir: &Path, bytes: &[u8]) -> Result<TempPathGuard> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create roster staging dir {}", dir.display()))?;
    let tmp = TempPathGuard::new(unique_temp_path(dir, "mcpmesh-roster-in"));
    let mut f = File::create(tmp.path())
        .with_context(|| format!("create temp roster {}", tmp.path().display()))?;
    f.write_all(bytes).context("write temp roster")?;
    f.sync_all().context("sync temp roster")?;
    Ok(tmp)
}

/// Handle an `org_join` control request: pin the org root + user key in config so
/// `status` shows the pinned fingerprint and the eventual roster install has its trust anchor.
/// No roster is installed by a join; the gate stays empty until the next roster arrives. Serialized under
/// `mesh.reload_lock` (the same config-write discipline as `register_service` / `install_roster`).
///
/// The pinned pk is parse-validated BEFORE the write (a garbage anchor must NOT land in config);
/// `user_key` is a LOCAL path (the key never crosses the API).
pub(crate) async fn org_join(
    state: &DaemonState,
    org_id: String,
    org_root_pk: String,
    user_id: String,
    user_key: String,
) -> Result<OrgJoinResult> {
    // Validate the pinned pk parses BEFORE writing (a garbage anchor must not land in config).
    crate::roster::parse_org_root_pk(&org_root_pk)?;
    let mesh = state.mesh_required()?;
    let _reload = mesh.reload_lock.lock().await;
    let config_path = mesh.config_path.clone();
    let (oid, pk, uid, uk) = (org_id.clone(), org_root_pk, user_id, user_key);
    blocking("join org-root pin config write", move || {
        write_join_pin(&config_path, &oid, &pk, &uid, &uk)
    })
    .await??;
    // #93b: the pin is durable, but roster mode is a BOOT decision — the ALPN set and the
    // gossip/presence/blob transport were fixed when this process started. A node that booted in
    // pairing mode is now half-live: its gate will accept org members as soon as a roster arrives,
    // while presence stays empty and blobs hard-close. Say so, rather than returning a bare success
    // that reads as "fully joined". Same shape #53 established for persisted-but-not-live.
    let restart_required = !mesh.roster_transport_live();
    if restart_required {
        tracing::warn!(
            org_id = %org_id,
            "org root pinned, but this node booted without the roster transport — presence and \
             app-blobs need a restart to take effect"
        );
    } else {
        tracing::info!(org_id = %org_id, "pinned org root (join)");
    }
    Ok(OrgJoinResult {
        org_id,
        restart_required,
    })
}

/// Handle a `set_nickname` control request (#37): validate, persist
/// `[identity].nickname` under `reload_lock` (the SAME serialization as
/// `register_service` — no lost-update window against a concurrent grant), and only on
/// write success install the new in-memory name future invites present. No restart, no
/// accept-loop reload (the nickname gates nothing). Display-only: peers keep their stored
/// pairing-time nickname until a re-invite.
pub(crate) async fn set_nickname(state: &DaemonState, nickname: String) -> Result<()> {
    let mesh = state.mesh_required()?;
    let nickname = nickname.trim().to_string();
    anyhow::ensure!(!nickname.is_empty(), "set_nickname: the nickname is empty");
    // The nickname becomes the peer-side `<peer>/<service>` mount prefix — a `/` would
    // make every mount of this node unparseable there.
    anyhow::ensure!(
        !nickname.contains('/'),
        "set_nickname: the nickname must not contain '/'"
    );
    let _reload = mesh.reload_lock.lock().await;
    let config_path = mesh.config_path.clone();
    let persisted = nickname.clone();
    blocking("join set_nickname config write", move || {
        write_identity_nickname(&config_path, &persisted)
    })
    .await??;
    mesh.set_self_nickname(nickname);
    Ok(())
}

/// Handle a `set_app_metadata` control request (#39): validate the ≤256B cap and store the
/// blob in memory; future presence heartbeats carry it (signed). `""` clears it. Roster mode
/// only for VISIBILITY (the beat is roster-only) — but the store itself is unconditional, so a
/// pure-pairing daemon accepts + holds it without error (it simply never gossips). No config
/// write, no reload: this is ephemeral runtime state, not durable trust/identity.
pub(crate) async fn set_app_metadata(state: &DaemonState, metadata: String) -> Result<()> {
    let mesh = state.mesh_required()?;
    let cap = crate::roster::presence::APP_METADATA_MAX_BYTES;
    anyhow::ensure!(
        metadata.len() <= cap,
        "set_app_metadata: metadata is {} bytes; the cap is {cap}",
        metadata.len()
    );
    mesh.set_app_metadata(metadata);
    Ok(())
}

/// Handle a `set_roster_url` control request: pin `[roster].url` in config AND, when
/// this node is in roster mode, (re)start the HTTPS poll loop AT RUNTIME so polling begins WITHOUT a
/// daemon restart. Written by `org create --roster-url` (the operator keeps the URL current) and by
/// `join` when the org invite carries one — the joiner's FIRST-roster bootstrap.
///
/// **Runtime poll (re)start (DECLARED).** `join` runs against an ALREADY-RUNNING daemon
/// that started pure-pairing (its step-5c poll spawn keyed on the STARTUP `roster_mode` snapshot).
/// The preceding `OrgJoin` pins `org_root_pk`, so once THIS write lands the LIVE config is in roster
/// mode. `poll_roster_url_once` reads the org anchor from config LIVE and is pure outbound HTTPS +
/// gate hot-swap (no endpoint/ALPN changes), so a runtime [`respawn_poll_loop`] genuinely starts
/// fetching — a fresh joiner installs its first roster autonomously (no human restart). The respawn
/// aborts+replaces any prior loop, so this is idempotent (repeated calls / a URL change never stack
/// loops). The operator's self-poll of its own URL is benign (equal-serial → confirm; a not-yet-hosted
/// URL → a logged Err retried next interval; a stale serial → `StaleSerial` skip — never a downgrade).
///
/// The config write is serialized under `mesh.reload_lock` (the SAME single-writer discipline as
/// `org_join` / `install_roster` / `register_service`); the respawn runs AFTER the write is durable and
/// takes only the short-lived `poll_loop` lock. The URL value is NOT logged (out of the trace surface).
pub(crate) async fn set_roster_url(state: &DaemonState, url: String) -> Result<()> {
    let mesh = state.mesh_required()?;
    let _reload = mesh.reload_lock.lock().await;
    let config_path = mesh.config_path.clone();
    let url_w = url.clone();
    blocking("roster-url config write", move || {
        write_roster_url(&config_path, &url_w)
    })
    .await??;
    tracing::info!("pinned roster url");
    // Roster mode (an org root is pinned in the LIVE config — the `OrgJoin` before this write set it
    // on a joiner, or `org create` on an operator): (re)start the poll loop NOW so a runtime join
    // bootstraps its first roster without a restart. Read live: a transient config-read error
    // just skips the runtime start (the next daemon start's step 5c picks it up), never fails the write.
    if Config::load(&mesh.config_path)
        .map(|c| c.identity.org_root_pk.is_some())
        .unwrap_or(false)
    {
        respawn_poll_loop(mesh, url).await;
        tracing::info!("roster URL poll loop (re)started");
    }
    Ok(())
}

/// (Re)start the HTTPS roster-poll loop, ABORTING any prior one first so repeated
/// calls never STACK duplicate loops (the idempotency guard) and a URL CHANGE cleanly replaces the
/// old loop. Called from TWO sites onto the SAME tracked `mesh.poll_loop` handle: (1) `serve_forever`
/// step 5c at startup (roster mode with a pinned URL), and (2) `set_roster_url` at RUNTIME — so a
/// joiner that pins `[roster].url` on an already-running daemon starts polling IMMEDIATELY (its
/// first-roster bootstrap) rather than waiting for a restart. Reads the poll interval from the LIVE
/// config (default hourly on any read/parse error). The freshly spawned loop does an immediate startup
/// poll (so the first roster arrives at once) then loops on the interval. Takes only the short-lived
/// `poll_loop` lock for the swap (never `reload_lock`, so no lock-order coupling with the config write
/// that precedes it in `set_roster_url`).
pub(crate) async fn respawn_poll_loop(mesh: &Arc<MeshState>, url: String) {
    let interval = Config::load(&mesh.config_path)
        .map(|c| c.roster.poll_interval_seconds())
        .unwrap_or(3600);
    let mut guard = mesh.poll_loop.lock().await;
    if let Some(old) = guard.take() {
        old.abort();
    }
    *guard = Some(crate::roster::distribute::spawn_poll_loop(
        mesh.clone(),
        url,
        interval,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::roster_status;
    use crate::daemon::testutil::hermetic_mesh;
    use crate::pairing;
    use ed25519_dalek::SigningKey;
    use mcpmesh_trust::roster::encode_b64u;

    /// `org_join` validates the pk BEFORE writing (garbage never lands), pins the four identity keys
    /// (preserving `nickname`), and flips `roster_status` from `None` (pure-pairing) to `"pending"`
    /// with serial 0 + the pinned org-root fingerprint.
    #[tokio::test(flavor = "multi_thread")]
    async fn org_join_pins_and_status_reports_pending() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[network]\nrelay_mode = \"disabled\"\n\n[identity]\nnickname = \"mydev\"\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = DaemonState::with_mesh("0.0.0", mesh.clone());
        // `roster_status` takes the caller's already-loaded config (L6: status loads it once);
        // the test mirrors control.rs `status_result` by loading fresh at each check.
        let live_status =
            |mesh: &Arc<MeshState>| roster_status(mesh, Config::load(&config_path).ok().as_ref());

        // Pure-pairing (no org pinned) → no roster block.
        assert!(
            live_status(&mesh).is_none(),
            "an unpinned daemon must surface no roster status"
        );

        // A GARBAGE anchor is rejected BEFORE the write — nothing lands in config.
        assert!(
            org_join(
                &state,
                "acme".into(),
                "not-a-real-key".into(),
                "alice".into(),
                "/home/alice/user.key".into(),
            )
            .await
            .is_err(),
            "a garbage org_root_pk must be rejected"
        );
        assert!(
            Config::load(&config_path)
                .unwrap()
                .identity
                .org_root_pk
                .is_none(),
            "a rejected join must NOT pin a garbage anchor"
        );
        assert!(
            live_status(&mesh).is_none(),
            "a rejected join leaves status unpinned"
        );

        // A VALID anchor pins the four keys + surfaces "pending".
        let root = SigningKey::from_bytes(&[9u8; 32]);
        let pk_str = encode_b64u(root.verifying_key().as_bytes());
        let res = org_join(
            &state,
            "acme".into(),
            pk_str.clone(),
            "alice".into(),
            "/home/alice/user.key".into(),
        )
        .await
        .expect("valid join pins");
        assert_eq!(
            res,
            OrgJoinResult {
                org_id: "acme".into(),
                // #93b: this hermetic mesh has no composed roster transport (gossip/blobs are
                // `None`), which is exactly the pairing-mode-node-joins-an-org case. The join must
                // SAY the node is half-live rather than answering a bare success — presence will
                // stay empty and blobs will hard-close until a restart, and nothing else on the
                // control surface reveals that.
                restart_required: true,
            }
        );

        // The four identity keys are pinned; the pre-existing nickname is preserved.
        let cfg = Config::load(&config_path).unwrap();
        assert_eq!(cfg.identity.org_id.as_deref(), Some("acme"));
        assert_eq!(cfg.identity.org_root_pk.as_deref(), Some(pk_str.as_str()));
        assert_eq!(cfg.identity.user_id.as_deref(), Some("alice"));
        assert_eq!(
            cfg.identity.user_key.as_deref(),
            Some(Path::new("/home/alice/user.key"))
        );
        assert_eq!(cfg.identity.nickname.as_deref(), Some("mydev"));

        // Status is now "pending": org pinned, NO roster installed → serial 0 + the fingerprint.
        let status = live_status(&mesh).expect("pinned org surfaces a pending status");
        assert_eq!(status.state, "pending");
        assert_eq!(status.serial, 0);
        assert_eq!(status.org_id, "acme");
        let expected_fp = pairing::sas::fingerprint_words(&root.verifying_key().to_bytes());
        assert!(!expected_fp.is_empty());
        assert_eq!(status.org_root_fingerprint, expected_fp);
    }

    /// CRITICAL: the URL poll must NOT panic. reqwest 0.13.4 (`rustls-no-provider`) resolves
    /// the rustls `CryptoProvider` at client-build time and PANICS if none is installed; iroh installs
    /// no process-default. This test installs the ring provider EXACTLY as `serve_forever` does, then
    /// drives `poll_roster_url_once` against an unreachable URL: it must return an `Err` (the failed
    /// GET), NOT panic. A panic (a missing provider) would ABORT the test — so a clean `Err` PROVES the
    /// `install_default` call is what keeps the poll alive. (Also exercises the fail-toward-degraded
    /// path: a blocked/unreachable poll is a plain error the loop logs + retries.)
    #[tokio::test(flavor = "multi_thread")]
    async fn url_poll_does_not_panic_when_the_ring_provider_is_installed() {
        // The SAME install `serve_forever` step 0 performs (idempotent — safe if another test ran it).
        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[network]\nrelay_mode = \"disabled\"\n").unwrap();
        let mesh = hermetic_mesh(config_path).await;

        // An unreachable URL (loopback port 1 — nothing listens → immediate connection refused). The
        // reqwest client BUILDS (the panic point) before the connection fails, so reaching this `Err`
        // proves the provider is installed. `reqwest::get` panics here WITHOUT the install.
        let result = crate::roster::distribute::poll_roster_url_once(
            &mesh,
            "http://127.0.0.1:1/roster.json",
        )
        .await;
        assert!(
            result.is_err(),
            "an unreachable poll must return Err (not panic, not a silent Ok): {result:?}"
        );
    }

    /// A std-only localhost HTTP/1.1 server that serves `body` for EVERY request until stopped.
    /// Returns `(url, stop_flag, join_handle)` — set the flag + join to shut it down. Non-blocking
    /// accept + poll so a clean shutdown never wedges the thread.
    fn serve_roster_http(
        body: Vec<u8>,
    ) -> (
        String,
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind http server");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut buf = [0u8; 2048];
                        let _ = stream.read(&mut buf);
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (format!("http://127.0.0.1:{port}/roster.json"), stop, handle)
    }

    /// **The runtime-bootstrap fix.** A `set_roster_url` on an ALREADY-RUNNING daemon whose config has the
    /// org anchor pinned but NO roster yet (the pending joiner, exactly the post-`OrgJoin` state) must
    /// (re)start the poll loop AT RUNTIME so the FIRST roster installs WITHOUT a daemon restart. Serve a
    /// valid signed serial-1 roster, call `set_roster_url`, and poll-assert (timeout + interval, NOT a
    /// fixed sleep) that the gate hot-swaps to serial 1 — proving the runtime respawn autonomously
    /// bootstraps the joiner (the no-human-restart directive). Idempotency: a SECOND `set_roster_url`
    /// aborts+replaces the loop (no stacking) and the roster stays installed.
    #[tokio::test(flavor = "multi_thread")]
    async fn runtime_set_roster_url_bootstraps_the_first_roster_without_a_restart() {
        use mcpmesh_trust::roster::mutate;
        use mcpmesh_trust::roster::sign::mint_signed;
        use std::sync::atomic::Ordering;
        // The SAME provider install `serve_forever` step 0 performs (idempotent).
        let _ = rustls::crypto::ring::default_provider().install_default();

        let root = SigningKey::from_bytes(&[9u8; 32]);
        let pk_str = encode_b64u(&root.verifying_key().to_bytes());
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        // A pending joiner's config: the org anchor is pinned (as `OrgJoin` sets it) but no roster.
        // `poll_interval` is pinned short: the assertion below must survive a transient failure of
        // the loop's immediate startup poll (the default interval is 1h — one flaky GET on a cold
        // CI runner would otherwise exhaust any reasonable deadline before the first retry).
        std::fs::write(
            &config_path,
            format!(
                "[network]\nrelay_mode = \"disabled\"\n[identity]\norg_root_pk = \"{pk_str}\"\norg_id = \"acme\"\n[roster]\npoll_interval = \"2s\"\n"
            ),
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        assert!(
            mesh.roster.view().is_none(),
            "pending joiner: no roster installed yet (D5)"
        );
        let state = DaemonState::with_mesh("0.0.0", mesh.clone());

        // A valid signed serial-1 roster hosted over HTTP (what the operator publishes to the URL).
        let now = epoch_now_i64();
        let roster = mint_signed(
            &root,
            mutate::empty_roster("acme", 1, now - 3600, now + 86_400),
        );
        let (url, stop, handle) = serve_roster_http(serde_json::to_vec(&roster).unwrap());

        // The RUNTIME trigger — no restart: pin the URL → `set_roster_url` respawns the poll loop →
        // the loop's immediate startup poll GETs + converges the first roster through install_from_file.
        set_roster_url(&state, url.clone())
            .await
            .expect("set_roster_url pins the URL and (re)starts polling");

        // Poll-assert the first roster installs autonomously (timeout + interval, never a fixed sleep).
        // Deadline widened for full-workspace parallelism AND cold CI runners: convergence is fast in
        // isolation, but CPU starvation — many concurrent test binaries, or a cold windows runner still
        // warming up — can slow the real HTTP/mesh path well past a tight bound. The wide bound
        // tolerates that starvation without masking a real hang: a genuine hang still fails at the bound.
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        loop {
            if mesh.roster.view().map(|v| v.serial()).unwrap_or(0) >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the first roster must install at RUNTIME (no daemon restart) after set_roster_url"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            mesh.roster.view().expect("roster installed").serial(),
            1,
            "the runtime poll bootstrapped the joiner's first roster (D5) — gate hot-swapped to serial 1"
        );

        // Idempotency: a SECOND set_roster_url aborts+replaces the loop (no stacking) — still serial 1.
        set_roster_url(&state, url)
            .await
            .expect("a repeat set_roster_url is idempotent (abort+replace, no stacking)");
        assert_eq!(
            mesh.roster.view().unwrap().serial(),
            1,
            "a repeat set_roster_url does not regress or double-install the roster"
        );

        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    #[test]
    fn staleness_sweep_fires_only_when_degraded_stopped() {
        use mcpmesh_trust::roster::validate::RosterState;
        assert!(super::should_staleness_sever(Some(
            RosterState::DegradedStopped
        )));
        assert!(!super::should_staleness_sever(Some(
            RosterState::DegradedGrace
        )));
        assert!(!super::should_staleness_sever(Some(RosterState::Approved)));
        assert!(
            !super::should_staleness_sever(None),
            "no roster → never sweep"
        );
    }
}

#[cfg(test)]
mod org_join_liveness_tests {
    use super::org_join;
    use crate::daemon::MeshState;
    use crate::daemon::testutil::hermetic_mesh;
    use mcpmesh_trust::roster::encode_b64u;
    use std::sync::Arc;

    /// A mesh with the roster transport ACTUALLY composed — a gossip handle on a real endpoint,
    /// which is what `compose_roster_transport` builds when a node boots with an org root pinned.
    async fn mesh_with_roster_transport(config_path: std::path::PathBuf) -> Arc<MeshState> {
        let dir = config_path.parent().unwrap();
        let store = Arc::new(crate::allowlist::PeerStore::open(&dir.join("s2.redb")).unwrap());
        let pairs = Arc::new(crate::allowlist::AllowlistGate::new(store.clone()));
        let roster = Arc::new(crate::roster::gate::RosterGate::empty());
        let gate: Arc<dyn mcpmesh_net::TrustGate> = Arc::new(
            crate::roster::gate::ComposedGate::new(roster.clone(), pairs),
        );
        let hermetic = crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            ..Default::default()
        };
        let endpoint = crate::daemon::boot::build_endpoint(
            iroh::SecretKey::from_bytes(&[31u8; 32]),
            &hermetic,
            true,
            None,
        )
        .await
        .unwrap();
        let gossip = crate::roster::transport::spawn_gossip(&endpoint);
        MeshState::new(
            endpoint,
            gate,
            store,
            Arc::new(crate::pairing::LiveInvites::new()),
            "test".into(),
            config_path,
            roster,
            Arc::new(mcpmesh_net::registry::ConnRegistry::new()),
            Some(gossip),
            None,
            None,
            None,
        )
    }

    /// #93(b): `org_join` must report when the join is only HALF live.
    ///
    /// Roster mode is a boot-time decision — it fixes the bound ALPN set and whether gossip,
    /// presence and app-blobs exist at all — while the roster GATE hot-swaps. So a node that booted
    /// in pairing mode and then joins reaches a state where MCP sessions to org members start
    /// working while presence stays permanently empty and blobs hard-close. It succeeded, partially,
    /// with no error anywhere.
    ///
    /// Both branches are asserted, because either alone is satisfied by a constant: a hardcoded
    /// `true` would tell every joiner to restart (including one that booted in roster mode and
    /// needs nothing), and a hardcoded `false` is the bug.
    #[tokio::test(flavor = "multi_thread")]
    async fn org_join_reports_whether_the_roster_transport_is_actually_running() {
        let root = mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let pk = encode_b64u(root.verifying_key().as_bytes());

        // (a) A node that booted in PAIRING mode: no transport composed → the join is half-live.
        let d1 = tempfile::tempdir().unwrap();
        let c1 = d1.path().join("config.toml");
        std::fs::write(&c1, "").unwrap();
        let m1 = hermetic_mesh(c1).await;
        assert!(
            !m1.roster_transport_live(),
            "a pairing-mode mesh composes no gossip — this is the precondition, not the assertion"
        );
        let s1 = crate::control::DaemonState::with_mesh("test", m1);
        let r1 = org_join(&s1, "acme".into(), pk.clone(), "alice".into(), "/k".into())
            .await
            .expect("the pin itself succeeds either way");
        assert!(
            r1.restart_required,
            "a node whose roster transport is not running must SAY the join needs a restart — \
             nothing else on the control surface reveals that presence will stay empty"
        );

        // (b) A node that booted WITH the transport: the join is fully in effect.
        let d2 = tempfile::tempdir().unwrap();
        let c2 = d2.path().join("config.toml");
        std::fs::write(&c2, "").unwrap();
        let m2 = mesh_with_roster_transport(c2).await;
        assert!(
            m2.roster_transport_live(),
            "precondition: gossip is composed"
        );
        let s2 = crate::control::DaemonState::with_mesh("test", m2);
        let r2 = org_join(&s2, "acme".into(), pk, "alice".into(), "/k".into())
            .await
            .expect("valid join pins");
        assert!(
            !r2.restart_required,
            "a node that already runs the roster transport must NOT tell its user to restart"
        );
    }
}
