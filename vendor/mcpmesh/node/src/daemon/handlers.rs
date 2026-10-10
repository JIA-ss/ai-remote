//! Control-verb handlers for the local `mcpmesh-local/1` API: service registration, peer
//! add/remove/rename, invite minting and redemption, the pairing grant/revoke pair, the
//! app-blob verbs, and the `open_session` dial-and-pipe — each one serialized against the
//! others through `MeshState::reload_lock` wherever it mutates config.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use mcpmesh_local_api::{
    BlobFetchCancelResult, BlobFetchResult, BlobPublishResult, BlobScopeList, InviteResult,
    PairResult, PeerAddParams, PeerRemoveParams, PeerRenameParams, RegisterServiceParams,
    ScopeInfo, SetRelaysResult,
};
use mcpmesh_net::errors::{ERR_UNREACHABLE, synthesized};
use mcpmesh_net::framing::{FrameReader, write_frame};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::allowlist::{PeerEntry, PeerStore};
use crate::audit::{AuditRecord, now_ts};
use crate::config::Config;
use crate::control::DaemonState;
use crate::pairing::Invite;
use crate::util::{blocking, epoch_now_u64};

use super::accept::swap_services;
use super::config_write::{
    append_allow_to_config, remove_allow_from_config, remove_principal_from_service,
    remove_service_from_config, write_relays, write_service_to_config,
};
use super::{MeshState, pipe_session};

/// A minted pairing invite lives at most 24h.
const INVITE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Cap on how long `mint_invite` waits for the endpoint to come "online" (a home-relay
/// handshake) before minting, so the invite's address carries the relay URL the redeemer
/// bootstraps from across NAT. It is a CAP, not a
/// fixed wait: production returns the instant the relay handshake completes (~1s). On the
/// relay-disabled localhost preset `online()` never completes, so this fires and we mint
/// with the direct-address-only addr (dialable on localhost/LAN — sufficient for tests).
///
/// **It must EXCEED iroh's own probe window, and 3s did not (#125).** `online()` cannot resolve
/// until iroh's net-report picks a home relay, and that report waits for its slowest probe under
/// `net_report::defaults::PROBES_TIMEOUT` — which is `Duration::from_secs(3)` in iroh 1.0.3, the
/// value this constant used to hold exactly.
///
/// Measured on a custom relay list containing one blackholed entry: `online()` resolved at
/// 3007–3021ms in every sample, i.e. just past a 3000ms deadline. So a node whose pinned relay was
/// dead lost that race essentially always and minted invites carrying only direct addresses — the
/// relay URL a WAN redeemer bootstraps from was silently absent, on a node that WAS online via the
/// healthy relays behind the dead one. That is a far worse outcome than a slow mint, and it is a
/// candidate mechanism for the multi-minute mesh-up #125 reported.
///
/// 5s clears iroh's window with margin. The cost is paid ONLY when no relay answers at all, where
/// the mint was already going to be relay-less; a healthy node returns in ~200ms either way. If a
/// future iroh raises `PROBES_TIMEOUT`, raise this with it — `cli/tests/relay_race.rs` pins the
/// ordering.
pub const RELAY_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle a `blob_publish` control request: add a LOCAL file into a scope on the gated
/// app-blob store, returning the ticket + hash. Requires roster mode (the provider is built only
/// there); a pure-pairing daemon answers a clean error.
pub(crate) async fn blob_publish(
    state: &DaemonState,
    scope: String,
    path: String,
) -> Result<BlobPublishResult> {
    let mesh = state.mesh_required()?;
    let provider = mesh.app_blobs().await.context(
        "app-blob provider not enabled (its store failed to build — check the daemon log)",
    )?;
    let (ticket, hash) = provider
        .publish_scope(&scope, Path::new(&path))
        .await
        .context("publish blob into scope")?;
    Ok(BlobPublishResult { ticket, hash })
}

/// Handle a `blob_grant` control request: grant a scope to a principal (single-writer).
pub(crate) async fn blob_grant(
    state: &DaemonState,
    scope: String,
    principal: String,
) -> Result<()> {
    let mesh = state.mesh_required()?;
    // Stored VERBATIM (#38), like service `allow`: a `b64u:`/`eid:` principal or a roster
    // group/user_id name grants; a bare display nickname does not authorize anyone.
    let provider = mesh.app_blobs().await.context(
        "app-blob provider not enabled (its store failed to build — check the daemon log)",
    )?;
    provider.grant(&scope, &principal)
}

/// Handle a `blob_revoke` control request (#62): withdraw principals from ONE scope's grants.
///
/// The blob analogue of #44 — un-sharing a file must not require unpairing the person. SCOPED, so
/// a principal's grants on other scopes are untouched; the global sweep is unpair hygiene and stays
/// reachable only through `peer_remove`.
pub(crate) async fn blob_revoke(
    state: &DaemonState,
    scope: String,
    principals: Vec<String>,
) -> Result<()> {
    let mesh = state.mesh_required()?;
    let provider = mesh.app_blobs().await.context(
        "app-blob provider not enabled (its store failed to build — check the daemon log)",
    )?;
    // An UNKNOWN scope is an error, not a silent ack (#62 review). Answering `{}` to a typo'd
    // scope tells an operator that access was withdrawn when nothing was touched — the exact defect
    // #55/#69 was filed about and fixed with `-32040`. "The principal was not granted" IS
    // idempotent and stays a clean success; "there is no such scope" is not.
    if !provider.has_scope(&scope) {
        anyhow::bail!(NoSuchBlobScope(scope));
    }
    let changed = provider.revoke_from_scope(&scope, &principals)?;
    tracing::info!(%scope, count = principals.len(), changed, "blob grants revoked");
    Ok(())
}

/// Handle a `blob_unpublish` control request (#62): remove a hash from ONE scope.
///
/// Takes effect IMMEDIATELY for authorization — the scope gate requires the hash to be listed in
/// some scope — but does NOT delete bytes synchronously. The local store keeps them until a
/// background sweep reclaims them, and only a node that set `[blobs].gc_interval` runs one (#80);
/// there is still no reclaim VERB, because `iroh_blobs` exposes no on-demand GC. A hash published
/// into several scopes stays reachable through the others — and stays LIVE for the sweep.
pub(crate) async fn blob_unpublish(state: &DaemonState, scope: String, hash: String) -> Result<()> {
    let mesh = state.mesh_required()?;
    let provider = mesh.app_blobs().await.context(
        "app-blob provider not enabled (its store failed to build — check the daemon log)",
    )?;
    // Parse the hash before touching anything (#62 review). Stored hashes are lowercase hex; an
    // UPPERCASE rendering of the same blake3 hash is valid, common, and would silently miss the
    // set removal — returning success while the blob stayed fetchable. Parsing normalizes it and
    // rejects garbage outright rather than acking a no-op.
    let parsed = crate::blobs::parse_blob_hash(&hash)?;
    let hash_hex = parsed.to_hex().to_string();
    if !provider.has_scope(&scope) {
        anyhow::bail!(NoSuchBlobScope(scope));
    }
    let changed = provider.unpublish(&scope, &hash_hex).await?;
    tracing::info!(%scope, changed, "blob unpublished from scope");
    Ok(())
}

/// The named blob scope does not exist (#62). A distinct error type so `respond` maps it to
/// [`ERR_NO_SUCH_SERVICE`](mcpmesh_local_api::ERR_NO_SUCH_SERVICE) — the same "you named something
/// that is not there" contract `service_allow_grant`/`_revoke` use — rather than acking a no-op.
#[derive(Debug)]
pub struct NoSuchBlobScope(pub String);

impl std::fmt::Display for NoSuchBlobScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no blob scope named '{}' — 'blob_list' shows the scopes this daemon has",
            self.0
        )
    }
}
impl std::error::Error for NoSuchBlobScope {}

/// The blob is not present COMPLETE in this daemon's local store (#83, `blob_republish`).
///
/// Distinct from [`NoSuchBlobScope`] because the remedy differs: a missing scope is a typo or an
/// unshared name, a missing blob means "fetch it first". Partial bytes report as missing too — an
/// interrupted fetch leaves them, and advertising a hash we cannot fully serve would turn the
/// original sender going offline into a hang at every fetcher.
#[derive(Debug)]
pub struct NoSuchBlob(pub String);

impl std::fmt::Display for NoSuchBlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "blob '{}' is not held complete by this daemon — fetch it before republishing",
            self.0
        )
    }
}
impl std::error::Error for NoSuchBlob {}

/// The blob was deliberately WITHDRAWN from this scope (#107) — `blob_unpublish` was called, and
/// `blob_republish` must not silently resurrect it.
///
/// Distinct from [`NoSuchBlob`] because the remedies are opposites: `NoSuchBlob` means "fetch it
/// first", this means "someone withdrew this on purpose; re-publish from the file if you mean it".
#[derive(Debug)]
pub struct BlobWithdrawn {
    pub scope: String,
    pub hash: String,
}

impl std::fmt::Display for BlobWithdrawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "blob '{}' was withdrawn from scope '{}' — republishing will not restore it; \
             'blob_publish' from the file if the re-share is intended",
            self.hash, self.scope
        )
    }
}
impl std::error::Error for BlobWithdrawn {}

/// Handle a `blob_republish` control request (#83): make a blob this daemon ALREADY holds servable
/// from here, in a scope it controls. No filesystem round-trip and no third copy of the bytes.
pub(crate) async fn blob_republish(
    state: &DaemonState,
    scope: String,
    hash: String,
) -> Result<mcpmesh_local_api::BlobPublishResult> {
    let mesh = state.mesh_required()?;
    let provider = mesh.app_blobs().await.ok_or_else(|| {
        anyhow::anyhow!(
            "app-blob provider not enabled (its store failed to build — check the daemon log)"
        )
    })?;
    // Return the CANONICAL hash, not the caller's rendering — `blob_publish` returns canonical
    // hex, and the docs promise the two are interchangeable.
    let (ticket, hash) = provider.republish(&scope, &hash).await?;
    tracing::info!(%scope, %hash, "blob republished");
    Ok(mcpmesh_local_api::BlobPublishResult { ticket, hash })
}

/// Handle a `blob_list` control request: the daemon's scopes (name → hashes + grants).
pub(crate) async fn blob_list(
    state: &DaemonState,
    params: mcpmesh_local_api::BlobListParams,
) -> Result<BlobScopeList> {
    let mesh = state.mesh_required()?;
    let q = crate::blobs::scope::ListQuery {
        scope: params.scope,
        hash: params.hash,
        limit: params.limit,
        offset: params.offset,
        counts_only: params.counts_only,
    };
    let Some(provider) = mesh.app_blobs().await else {
        return Ok(BlobScopeList {
            scopes: Vec::new(),
            total: 0,
            truncated: false,
        });
    };
    let page = provider.list_page(&q)?;
    Ok(BlobScopeList {
        scopes: page
            .rows
            .into_iter()
            .map(
                |(name, hashes, grants, withdrawn, hash_count, grant_count, withdrawn_count)| {
                    ScopeInfo {
                        name,
                        hashes,
                        grants,
                        withdrawn,
                        hash_count,
                        grant_count,
                        withdrawn_count,
                    }
                },
            )
            .collect(),
        total: page.total,
        truncated: page.truncated,
    })
}

/// A request stopped on purpose before it finished (#172). A distinct error type so `respond` maps
/// it to [`ERR_CANCELLED`](mcpmesh_local_api::ERR_CANCELLED) rather than the generic `-32000`: the
/// caller asked for this, and "your fetch stopped because you cancelled it" is not a failure a
/// client should surface as one.
#[derive(Debug)]
pub struct Cancelled(pub String);

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cancelled by blob_fetch_cancel: {} — partial chunks stay in the store, unlisted and \
             unreclaimable (#80), exactly as they do when a fetch fails",
            self.0
        )
    }
}

impl std::error::Error for Cancelled {}

/// Registers an in-flight fetch in [`MeshState::fetches`] and DEREGISTERS on drop (#172).
///
/// Drop, not an explicit call at the end of `blob_fetch`: the fetch has several `?` early returns
/// and a `select!` arm, and a leaked registration is worse than no registration at all — the hash
/// would look permanently in-flight, so `blob_fetch_cancel` would answer `cancelled: true` for a
/// fetch that ended long ago, and the next real fetch of that hash would inherit an
/// already-tripped token and cancel itself instantly.
struct FetchGuard {
    mesh: Arc<MeshState>,
    hash: String,
    token: crate::cancel::CancelToken,
    /// The provider, kept only so an UNFINISHED fetch can emit its terminal `Aborted` frame.
    provider: Arc<crate::blobs::provider::AppBlobs>,
    /// Set once the fetch has answered on its own. While false, dropping means "stopped".
    finished: bool,
}

impl FetchGuard {
    fn register(
        mesh: &Arc<MeshState>,
        hash: String,
        provider: Arc<crate::blobs::provider::AppBlobs>,
    ) -> Self {
        let token = {
            let mut map = mesh.fetches.lock().expect("fetches lock not poisoned");
            let slot = map
                .entry(hash.clone())
                .or_insert_with(|| crate::daemon::FetchSlot {
                    token: crate::cancel::CancelToken::new(),
                    waiters: 0,
                });
            // A token that has ALREADY been tripped must not be inherited. Cancel-then-retry is the
            // ordinary case — a user clicks Cancel and then Retry — and the cancelled fetch's guard
            // does not deregister until its future is actually polled and dropped. Registering
            // against the dead token in that window made the retry answer "cancelled" instantly,
            // for no reason the user could see. `waiters` still counts LIVE registrations, so the
            // winding-down sibling's drop decrements correctly against the new token's slot.
            if slot.token.is_cancelled() {
                slot.token = crate::cancel::CancelToken::new();
            }
            slot.waiters += 1;
            slot.token.clone()
        };
        Self {
            mesh: mesh.clone(),
            hash,
            token,
            provider,
            finished: false,
        }
    }
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        {
            let mut map = self.mesh.fetches.lock().expect("fetches lock not poisoned");
            if let Some(slot) = map.get_mut(&self.hash) {
                slot.waiters = slot.waiters.saturating_sub(1);
                // Only the LAST fetch of this hash removes the entry. Removing unconditionally
                // would drop a token a concurrent sibling is still watching, leaving that sibling
                // running and uncancellable.
                if slot.waiters == 0 {
                    map.remove(&self.hash);
                }
            }
        }
        // A fetch that stopped without answering owes its subscribers a terminal frame. Emitted
        // from DROP rather than from the cancel arm so it covers both ways a fetch can be stopped:
        // `blob_fetch_cancel`, and the control connection closing (which aborts the task — and an
        // aborted future still runs its destructors).
        if !self.finished {
            self.provider.emit_fetch_aborted(&self.hash);
        }
    }
}

/// Handle a `blob_fetch` control request: fetch a `mcpmesh/blob/1` ticket THROUGH the daemon
/// (BLAKE3-verified streaming into the gated store) and export the verified blob to `dest_path` (a
/// local file the same-uid daemon writes — within the trust boundary). Returns the verified hash + byte length.
///
/// Cancellable (#172) via [`blob_fetch_cancel`], keyed by the ticket's hash — which is read up
/// front so the dial itself is inside the cancellable region, not just the transfer after it.
pub(crate) async fn blob_fetch(
    state: &DaemonState,
    ticket: String,
    dest_path: String,
    from: Vec<String>,
) -> Result<BlobFetchResult> {
    let mesh = state.mesh_required()?;
    let provider = mesh.app_blobs().await.context(
        "app-blob provider not enabled (its store failed to build — check the daemon log)",
    )?;
    let hash_hex = crate::blobs::provider::AppBlobs::ticket_hash(&ticket)?
        .to_hex()
        .to_string();
    let mut guard = FetchGuard::register(mesh, hash_hex.clone(), provider.clone());
    let dest = PathBuf::from(dest_path);
    let work = async {
        // #83: resolve the caller's alternate sources to dialable addresses BEFORE the fetch, so a
        // name that resolves to nobody is a clean error rather than a silent skip mid-transfer.
        let sources = crate::daemon::dial::blob_source_addrs(mesh, &from).await?;
        // #223: the ticket's publisher is source 0 of a real dial, and it is a REMOTE claim — a
        // ticket naming a device this node revoked must not make the fetch contact it. The
        // alternates were filtered above; this is the same predicate for the one source they skip.
        let publisher = crate::blobs::provider::AppBlobs::ticket_publisher(&ticket)?;
        let m = mesh.clone();
        let publisher_refused =
            crate::util::blocking("join blob publisher revocation", move || {
                crate::daemon::dial::dial_refused(&m, &publisher)
            })
            .await?;
        if publisher_refused {
            tracing::warn!(
                "blob ticket's publisher is REVOKED on this node; it will not be dialled"
            );
        }
        let hash = provider
            .fetch_from_sources(&ticket, &sources.addrs, !publisher_refused)
            .await
            .map_err(|e| {
                // The caller NAMED these sources, so a failed fetch says why they were not tried —
                // otherwise the only trace is a daemon log line the caller cannot see (#223).
                match sources.skipped_revoked {
                    0 => e,
                    n => e.context(format!(
                        "{n} named source{} skipped: revoked on this node",
                        if n == 1 { "" } else { "s" }
                    )),
                }
            })
            .context("fetch blob")?;
        // #80: pin the fetched blob across the EXPORT. It is in the store and in no scope — the
        // fetch creates no tag — so on a collecting node a sweep between the transfer completing
        // and the export finishing would reclaim it, and the caller would get a `NotFound` after a
        // fully successful transfer. Held only for the export; afterwards the blob is deliberately
        // reclaimable, since the caller's `dest_path` is the durable copy and the store's is a
        // cache (see `[blobs]` in docs/config.md).
        let _pin = provider.pin(hash).await;
        // STREAM to disk (#82). The previous `read_bytes` + `fs::write` held the entire blob in
        // memory before a byte landed, so peak RSS was blob-sized and a large fetch OOM-killed the
        // node rather than merely being slow. `export` writes incrementally and reports the size,
        // so nothing here scales with the blob.
        let bytes_len = provider.export_to(hash, &dest).await?;
        anyhow::Ok(BlobFetchResult {
            hash: hash.to_hex().to_string(),
            bytes_len,
        })
    };
    // Cooperative, not `abort()`: a cancelled fetch must still ANSWER, or the caller waits forever
    // on work that has already stopped. Dropping `work` here is what actually halts the transfer —
    // the iroh-blobs get stream and the export both stop when their future is dropped.
    let outcome = tokio::select! {
        // Set INSIDE this arm, not after the select (#182). The fetch ANSWERED here — success or a
        // genuine transfer failure, both of which the provider has already reported terminally on
        // the transfer ring, so the guard must not emit a second frame.
        //
        // Setting it after the select covered the CANCEL arm too, and there the claim is false:
        // `work` was DROPPED, so nothing inside it emitted anything. `finished` was then true and
        // `Drop` skipped the frame — so an explicit `blob_fetch_cancel` left every subscriber
        // watching a progress bar that never terminated, which is exactly the case the guard's own
        // comment says it exists to cover.
        r = work => { guard.finished = true; r }
        () = guard.token.cancelled() => Err(Cancelled(hash_hex).into()),
    };
    outcome
}

/// Handle a `blob_fetch_cancel` control request (#172): trip the cancel token every in-flight
/// [`blob_fetch`] of `hash` is watching.
///
/// `cancelled: false` — nothing was fetching that hash here — is a normal answer, not an error. A
/// cancel that races a fetch to completion is indistinguishable from one for a hash that was never
/// fetched, and inventing an error for the race would make a UI report a failure for a transfer
/// that in fact succeeded.
pub(crate) fn blob_fetch_cancel(state: &DaemonState, hash: &str) -> Result<BlobFetchCancelResult> {
    let mesh = state.mesh_required()?;
    // Parse through the SAME validator every other hash-taking verb uses, and key the lookup off
    // its canonical rendering. The point is that a malformed hash is an ERROR: a raw map lookup
    // answered `cancelled: false` for a typo, which reads as "that transfer already finished" and
    // sends the caller looking in the wrong place while the fetch carries on. (It also future-
    // proofs the lookup against the 52-char base32 rendering `parse_blob_hash` accepts; the
    // iroh-blobs in this tree renders hex, so today the normalization itself is a no-op.)
    let key = crate::blobs::parse_blob_hash(hash)?.to_hex().to_string();
    let slot = mesh
        .fetches
        .lock()
        .expect("fetches lock not poisoned")
        .get(&key)
        .cloned();
    // The token is tripped OUTSIDE the map lock. `cancel()` wakes every waiter, and those waiters'
    // guards take this same lock as they unwind — holding it across the trip invites the
    // lock-ordering hazard for no benefit.
    match slot {
        Some(slot) => {
            slot.token.cancel();
            Ok(BlobFetchCancelResult { cancelled: true })
        }
        None => Ok(BlobFetchCancelResult { cancelled: false }),
    }
}

/// Reload the config from disk and hot-swap the LIVE service registry with services rebuilt from
/// it — the shared read→rebuild→swap tail of every config-mutating control verb
/// ([`register_service`], [`rename_peer`], [`grant_service_access`], [`revoke_service_access`]).
/// `why` names the mutation for the reload error (`"reload config after {why}: …"`). The CALLER
/// holds `mesh.reload_lock` around its whole critical section; [`swap_services`] itself takes only
/// the live handle's short write lock.
///
/// Post-#54: the swap is visible to connections that are ALREADY open (their next session
/// reads the new registry), not merely to connections accepted afterwards.
async fn reload_services_from_disk(mesh: &Arc<MeshState>, why: &str) -> Result<()> {
    let cfg = Config::load(&mesh.config_path)
        .map_err(|e| anyhow::anyhow!("reload config after {why}: {e}"))?;
    // Overlay the in-memory ephemeral registrations (#36) so they survive every hot-reload —
    // grants, renames, roster installs all funnel through here, so none of them drop an
    // ephemeral service. The lock is held only for the tiny clone.
    let ephemeral = mesh
        .ephemeral_services
        .lock()
        .expect("ephemeral_services lock not poisoned")
        .clone();
    swap_services(
        mesh,
        crate::daemon::build_services_with_ephemeral(
            &cfg,
            &mesh.audit(),
            &mesh.limits(),
            &ephemeral,
        ),
    );
    Ok(())
}

/// Handle a `register_service` control request: write/update the `[services.*]` config entry
/// (atomic), reload the registry, and hot-reload the mesh serve loop. Config writes block, so
/// they run on `spawn_blocking` (the fs house rule).
pub(crate) async fn register_service(
    state: &DaemonState,
    params: RegisterServiceParams,
) -> Result<()> {
    let mesh = state.mesh_required()?;

    // Serialize the ENTIRE critical section (read → upsert → write → reload → rebuild → serve
    // swap → status). Two concurrent registrations must not read the same base config and
    // clobber each other's new service. Held until this function returns.
    let _reload = mesh.reload_lock.lock().await;

    let RegisterServiceParams {
        name,
        backend,
        allow,
        ephemeral,
        rate_limit_per_min,
    } = params;
    // #63: `0` would silently block every request to this service. Refused, the same call
    // `max_uses` makes — a caller asking for zero has a bug, and honouring it hides one.
    if rate_limit_per_min == Some(0) {
        anyhow::bail!(crate::control::InvalidParams(
            "rate_limit_per_min must be at least 1 (omit it to use [limits].rate_limit_per_min)"
                .into()
        ));
    }
    // `allow` entries are stored VERBATIM (#38): a `b64u:`/`eid:` principal or a roster
    // group/user_id name admits; a bare display nickname does NOT (nicknames never authorize
    // — the daemon deliberately does no nickname→principal resolution here, since a
    // self-asserted nickname could shadow roster vocabulary and misdirect the grant, and a
    // non-unique nickname is ambiguous). Pairing GRANTS write the peer's principal directly
    // from its verified identity; a manual grant names a principal or a roster group. The
    // doctor lint flags a stray nickname on a pure-pairing node.

    if ephemeral {
        // #36: in-memory only. Refuse a name that already exists on disk — an ephemeral entry
        // must not silently shadow (or be shadowed by) a persistent one, and unregistering it
        // later must never touch config. A repeat ephemeral register of the same name updates it.
        let cfg = Config::load(&mesh.config_path)
            .map_err(|e| anyhow::anyhow!("config error in {}: {e}", mesh.config_path.display()))?;
        if cfg.services.contains_key(&name) {
            anyhow::bail!(
                "service '{name}' is already registered persistently in config; \
                 use a different name for an ephemeral registration"
            );
        }
        mesh.ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned")
            .insert(
                name.clone(),
                crate::daemon::EphemeralService {
                    backend,
                    allow: allow.clone(),
                    // Stored as REQUESTED; the clamp against `[limits].rate_limit_per_min` happens
                    // in `MeshLimiters::for_service`, so there is ONE place the ceiling is applied
                    // and the config and control paths cannot enforce it differently.
                    rate_limit_per_min,
                },
            );
        reload_services_from_disk(mesh, "register-ephemeral").await?;
        tracing::info!(service = %name, "registered ephemeral service");
        return Ok(());
    }

    // Persistent: refuse a name an EPHEMERAL registration already holds — the symmetric half of
    // the guard above (#55 review). Without it a config entry could be created UNDER a live
    // ephemeral one; the overlay would shadow it, so the allow verbs would mutate the ephemeral
    // copy while the config copy sat unreachable — and then went live, with a stale allow, the
    // moment the registering control connection dropped the ephemeral entry.
    {
        let map = mesh
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned");
        if map.contains_key(&name) {
            anyhow::bail!(
                "service '{name}' is currently registered ephemerally; \
                 unregister it first, or use a different name for the persistent registration"
            );
        }
    }

    // Persistent: atomic config write on a blocking thread, then hot-reload.
    let config_path = mesh.config_path.clone();
    let (name_w, backend_w, allow_w, rate_w) = (
        name.clone(),
        backend.clone(),
        allow.clone(),
        rate_limit_per_min,
    );
    blocking("join config write", move || {
        write_service_to_config(&config_path, &name_w, &backend_w, &allow_w, rate_w)
    })
    .await??;

    // Reload config, rebuild the registry from the persisted truth, and hot-reload: abort the
    // old accept loop, spawn a fresh one on the same endpoint carrying the rebuilt registry
    // (a brief serving blip is acceptable). Shared with the pairing grant / revoke / rename via
    // [`reload_services_from_disk`] (DRY). `status` reads the config live, so the new service is
    // visible on the very next call.
    reload_services_from_disk(mesh, "register").await?;

    tracing::info!(service = %name, "registered/updated service");
    Ok(())
}

/// Unregister the named EPHEMERAL services (#36) and hot-reload so the accept loop stops offering
/// them. Called when a control connection that registered ephemeral services closes. Persistent
/// (config) services are never touched. A no-op if nothing was ephemerally registered by the
/// connection. Takes `reload_lock`, like every registry mutation.
#[doc(hidden)]
pub async fn unregister_ephemeral(mesh: &Arc<MeshState>, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let _reload = mesh.reload_lock.lock().await;
    {
        let mut map = mesh
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned");
        for name in names {
            map.remove(name);
        }
    }
    if let Err(e) = reload_services_from_disk(mesh, "unregister-ephemeral").await {
        tracing::warn!(%e, "reload after ephemeral unregister failed");
    }
}

/// Handle a `peer_add` control request: write
/// a [`PeerEntry`] to the daemon's OPEN store (redb is single-process, so this must route
/// through the daemon). The live [`AllowlistGate`](crate::allowlist::AllowlistGate) reads the
/// same database, so the new peer is resolvable on the very next accept — no gate rebuild
/// needed — and `status` reads the store live, so it shows the peer immediately.
pub(crate) async fn add_peer(state: &DaemonState, params: PeerAddParams) -> Result<()> {
    let mesh = state.mesh_required()?;
    let PeerAddParams {
        nickname,
        endpoint_id,
        allow,
    } = params;
    // endpoint_id encoding = iroh's native base32 (`EndpointId`/`PublicKey` Display/FromStr,
    // `decode_base32_hex`); round-trips the 32 bytes and matches what pairing/status show.
    let endpoint_id = endpoint_id
        .parse::<iroh::EndpointId>()
        .map_err(|e| anyhow::anyhow!("peer_add: endpoint_id is not a valid EndpointId: {e}"))?;
    let entry = PeerEntry {
        endpoint_id: *endpoint_id.as_bytes(),
        nickname: nickname.clone(),
        services: allow,
        // `internal peer add` is not a pairing write — leave the audit stamp unset
        // (only the pair rendezvous records `paired_at`) and no pairing-proven dial hint
        // (`last_addr` — discovery resolves this peer).
        paired_at: None,
        user_id: None,
        last_addr: None,
    };

    // redb writes block + fsync — run on a blocking thread (the fs house rule).
    let store = mesh.store.clone();
    blocking("join peer add", move || store.add(entry)).await??;

    tracing::info!(peer = %nickname, "added peer to allowlist");
    Ok(())
}

/// The #86 hook that PERSISTS an adopted self-enrollment binding and installs it live.
///
/// The enrolled device cannot re-derive this — it holds no user key — so the file is the only copy.
/// Written before the ceremony returns, so a caller that sees `enrolled_as_self: true` can rely on
/// the identity surviving a restart.
pub(crate) fn adopt_hook(mesh: &Arc<MeshState>) -> crate::pairing::rendezvous::AdoptBindingFn {
    let mesh = mesh.clone();
    Box::new(move |binding: crate::pairing::rendezvous::SelfBinding| {
        let mesh = mesh.clone();
        Box::pin(async move {
            // #214: serialized with `self_enroll_detach` and `user_key_import`, the other writers
            // of this slot and its sidecar. Without it a detach racing an adoption could remove
            // the file, lose to the adopt's write, then clear the live slot — leaving a sidecar
            // the next boot re-adopts while `status` reports the exit took.
            let _guard = mesh.user_key_lock.lock().await;
            let path = mesh.adopted_binding_path();
            let json = serde_json::to_vec(&binding)?;
            blocking("join adopt binding write", move || {
                crate::pairing::persist::write_private(&path, &json)
            })
            .await??;
            // Live, so this device presents the adopted identity without a restart.
            mesh.set_self_binding_live(Some(binding.clone()));
            // #86 gate: the REDEEMER side audits too. Adopting an identity is a durable trust
            // change on this device — the one an operator asks about later.
            mesh.audit().record(crate::audit::AuditRecord::trust(
                now_ts(),
                "self_enroll_adopt".into(),
                Some(binding.user_pk.clone()),
                None,
            ));
            anyhow::Ok(())
        })
    })
}

/// The #86 gate for every path that SIGNS with this node's user key, and the lock it is checked
/// under (#221).
///
/// An ENROLLED device presents an identity it holds no key for, so signing with the LOCAL key boot
/// minted underneath produces something no peer can redeem. The check used to be a bare read of
/// `adopted_binding`, and the caller then read the key and signed — so an `adopt_hook` landing in
/// between passed the gate. Returning the `user_key_lock` guard from the check makes that
/// unrepresentable: a caller holding the result signs under the same lock hold the gate was
/// checked in, and `adopt_hook`/`user_key_import`/`self_enroll_detach` write under it.
///
/// **Bind the guard to a named variable** (`let _key_guard = …`) and keep it until the signature is
/// made — `let _ = …` drops it on the spot. Never hold it across a network await.
///
/// Refuses with `-32602` in the shape `peer_endorse` has always used.
pub(crate) async fn lock_own_user_key<'m>(
    mesh: &'m MeshState,
    verb: &str,
    action: &str,
) -> Result<tokio::sync::MutexGuard<'m, ()>> {
    let guard = mesh.user_key_lock.lock().await;
    anyhow::ensure!(
        mesh.adopted_binding
            .read()
            .expect("adopted_binding lock not poisoned")
            .is_none(),
        crate::control::InvalidParams(format!(
            "{verb}: this device was enrolled into another device's identity (#86) and does not \
             hold that user key. {action} from the device that does."
        ))
    );
    Ok(guard)
}

/// Handle a `peer_endorse` control request (#65): sign a statement vouching for `subject`, for a
/// third party to redeem with `peer_introduce`.
///
/// The other half of an introduction. Without it nothing can produce `evidence` and the install
/// half is unusable — which is what the first version shipped.
///
/// Signs with THIS node's user key, read from disk per request rather than held in memory — READ,
/// never minted: a missing key is refused (#221).
/// Endorsing changes nothing about our OWN trust in the subject: it is a statement for someone
/// else, and they decide what it is worth.
pub async fn endorse_peer(
    state: &DaemonState,
    params: mcpmesh_local_api::PeerEndorseParams,
) -> Result<mcpmesh_local_api::PeerEndorseResult> {
    let mesh = state.mesh_required()?;
    let subject_id = params
        .subject
        .strip_prefix("eid:")
        .unwrap_or(&params.subject)
        .parse::<iroh::EndpointId>()
        .map_err(|e| {
            crate::control::InvalidParams(format!(
                "peer_endorse: subject is not a valid endpoint id: {e}"
            ))
        })?;

    // #86 gate: an ENROLLED device holds no authority over the identity it presents. Signing with
    // its LOCAL key would return an `endorsed_by` no peer has ever paired with, so every
    // endorsement it produced would be silently unredeemable. #221: checked under
    // `user_key_lock`, held until the signature is made.
    let _key_guard = lock_own_user_key(mesh, "peer_endorse", "Endorse").await?;
    let subject_bytes = *subject_id.as_bytes();
    let subject_uid = params.subject_user_id.clone();
    mesh.run_identity_gate_hook().await;
    // READ, never `load_or_generate` (#221): a missing `user.key` used to be MINTED here, silently
    // replacing the identity this node presents on disk — and the next self-enrollment ceremony
    // then signed under the new key while presenting the old one. Refused instead, as
    // `device_revoke` and `user_key_export` do.
    let user_key = read_user_key(mesh).await?;
    let (endorsed_by, evidence) = blocking("join endorse", move || {
        let evidence =
            mcpmesh_trust::binding::endorse(&user_key, &subject_bytes, subject_uid.as_deref())?;
        anyhow::Ok((mcpmesh_trust::binding::user_id(&user_key), evidence))
    })
    .await??;

    Ok(mcpmesh_local_api::PeerEndorseResult {
        endorsed_by,
        evidence,
    })
}

/// Handle a `peer_introduce` control request (#65): install a peer vouched for by someone we are
/// ALREADY paired with, without a fresh two-human SAS ceremony.
///
/// **Installs IDENTITY, never AUTHORIZATION** — and the mechanism is the `user_id` discipline, not
/// the empty `services` list. `[services.*].allow` matches on principals: the subject's `eid:`, and
/// its `user_id` when it has one. An introduction can only set a `user_id` the SUBJECT proved with
/// its own device binding, so an endorser cannot hand the subject a victim's identity and with it
/// that victim's grants. Service access is principal-keyed in
/// config (#38) and stays an explicit, separate act. That is what bounds the whole feature: a
/// compromised endorser can make us KNOW about an attacker, it cannot make us SERVE one.
///
/// Unlike `peer_add` — reserved precisely because the caller merely ASSERTS an id — this is
/// verifiable: the endorsement is checked against a user key we already hold from pairing.
pub async fn introduce_peer(
    state: &DaemonState,
    params: mcpmesh_local_api::PeerIntroduceParams,
) -> Result<()> {
    let mesh = state.mesh_required()?;
    let mcpmesh_local_api::PeerIntroduceParams {
        subject,
        endorsed_by,
        evidence,
        subject_user_id,
        subject_binding,
        nickname,
    } = params;

    let nickname = validated_alias("nickname", Some(nickname))?
        .expect("Some in, Some out — the None arm is unreachable here");

    // Same encoding as `peer_add`/status: iroh's native base32.
    let subject_id = subject
        .strip_prefix("eid:")
        .unwrap_or(&subject)
        .parse::<iroh::EndpointId>()
        .map_err(|e| {
            crate::control::InvalidParams(format!(
                "peer_introduce: subject is not a valid endpoint id: {e}"
            ))
        })?;
    let subject_bytes = *subject_id.as_bytes();

    // Introducing OURSELVES is meaningless and would write a self-row the gate would then resolve.
    anyhow::ensure!(
        subject_bytes != *mesh.endpoint.id().as_bytes(),
        crate::control::InvalidParams("peer_introduce: that is this node's own endpoint id".into())
    );

    // THE TRUST-CHAIN CHECK. The endorser must be a peer we are CURRENTLY paired with, identified
    // by the `user_id` we stored when we paired (with a SAS). An endorsement from a stranger — or
    // from someone we have since unpaired — is refused, so the chain always terminates at a
    // ceremony the operator performed themselves.
    let store = mesh.store.clone();
    let endorser = endorsed_by.clone();
    let known = blocking("join endorser lookup", move || {
        anyhow::Ok(
            store
                .list()?
                .into_iter()
                // `paired_at.is_some()` is LOAD-BEARING, not tidiness: without it an INTRODUCED
                // peer qualifies as an endorser the moment it has a user_id, so introductions
                // chain transitively to unbounded depth and the chain no longer terminates at any
                // ceremony the operator performed. Demonstrated in review. Only the pairing path
                // stamps `paired_at`; `peer_add` writes `user_id: None` and so cannot mint one
                // either.
                .any(|e| e.paired_at.is_some() && e.user_id.as_deref() == Some(endorser.as_str())),
        )
    })
    .await??;
    anyhow::ensure!(
        known,
        crate::control::InvalidParams(
            "peer_introduce: endorsed_by is not the user_id of a peer you are currently paired \
             with — an introduction's trust chain must end at someone you paired with yourself"
                .into()
        )
    );

    mcpmesh_trust::binding::verify_endorsement(
        &endorsed_by,
        &evidence,
        &subject_bytes,
        subject_user_id.as_deref(),
    )
    .map_err(|e| {
        crate::control::InvalidParams(format!(
            "peer_introduce: the endorsement does not verify: {e}"
        ))
    })?;

    // A `user_id` is AUTHORIZATION-BEARING — `[services.*].allow` matches on it, and the trust gate
    // resolves it. The endorser vouching for it is NOT enough: a `user_id` is public (it is on
    // `status`, on `PairResult`, on every audit record), so an endorser could name a VICTIM's
    // user_id for an attacker's endpoint and the attacker would inherit that victim's grants —
    // the exact inverse of this feature's bound, demonstrated end to end in review.
    //
    // So the SUBJECT must prove the key is theirs, with the same device→user binding a peer
    // presents at pairing. Two independent signatures, saying different things: the endorser's
    // "I vouch for this endpoint", the subject's "this user key is mine".
    let verified_user_id = match (&subject_user_id, &subject_binding) {
        (None, None) => None,
        (Some(_), None) => anyhow::bail!(crate::control::InvalidParams(
            "peer_introduce: subject_user_id requires subject_binding — the SUBJECT must prove it \
             controls that user key, or an endorsement could name someone else's user_id and \
             inherit their grants"
                .into()
        )),
        (None, Some(_)) => anyhow::bail!(crate::control::InvalidParams(
            "peer_introduce: subject_binding without subject_user_id has nothing to bind".into()
        )),
        (Some(uid), Some(sig)) => {
            // BOUND to the subject's endpoint id, never a self-asserted one — a transplanted
            // binding for a different endpoint fails, exactly as at pairing.
            let proven = mcpmesh_trust::binding::verify_presented(uid, sig, &subject_bytes)
                .map_err(|e| {
                    crate::control::InvalidParams(format!(
                        "peer_introduce: the subject's device binding does not verify: {e}"
                    ))
                })?;
            Some(proven)
        }
    };

    // IDENTITY REVOCATION, before anything is written (#218) — the question the gate asks of every
    // row (`PeerStore::admission`) and redemption asks before its own write. A proven
    // `user_id` this node has revoked would land a row the gate refuses on sight, with the caller
    // told "installed". The same coded refusal as a grant to a revoked principal (#212): the same
    // fact about the same table, and the same remedy. Checked on the PROVEN id only — an
    // endorser cannot name one (above), so there is nothing else it could be checked on.
    if let Some(uid) = verified_user_id.clone() {
        let store = mesh.store.clone();
        let revoked = blocking("join introduce revocation check", {
            let uid = uid.clone();
            move || store.is_user_revoked(&uid)
        })
        .await?;
        if revoked {
            anyhow::bail!(PrincipalRevoked(uid));
        }
    }

    // The same display-uniqueness guard pairing runs, for the same reason: a duplicate nickname
    // makes our own `<peer>/<service>` routing ambiguous (#87).
    let store = mesh.store.clone();
    let (nick, subj) = (nickname.clone(), subject_bytes);
    let collides = blocking("join introduce collision check", move || {
        anyhow::Ok(
            store
                .list()?
                .into_iter()
                .any(|e| e.nickname == nick && e.endpoint_id != subj),
        )
    })
    .await??;
    anyhow::ensure!(
        !collides,
        crate::control::InvalidParams(format!(
            "peer_introduce: you already use the name '{nickname}' for a different peer"
        ))
    );

    // `PeerStore::add` is an UPSERT, so introducing a peer we already PAIRED with would replace a
    // proven row with a weaker one — destroying its verified `user_id`, its `paired_at` stamp and
    // its pairing-proven dial hint. That is the exact downgrade `set_last_addr` was rewritten to
    // prevent and that the pairing merge guards against. An already-paired peer is already trusted
    // more strongly than any endorsement can make it, so there is nothing to gain by allowing it.
    let store = mesh.store.clone();
    let already_paired = blocking("join introduce paired check", move || {
        anyhow::Ok(
            store
                .resolve(&subject_bytes)?
                .is_some_and(|e| e.paired_at.is_some()),
        )
    })
    .await??;
    anyhow::ensure!(
        !already_paired,
        crate::control::InvalidParams(
            "peer_introduce: you are already paired with that peer — an introduction would REPLACE \
             a row proven by a SAS ceremony with a weaker one"
                .into()
        )
    );

    let entry = PeerEntry {
        endpoint_id: subject_bytes,
        nickname: nickname.clone(),
        // Empty — but this is HYGIENE, not the security property, and the first version of this
        // comment claimed otherwise. `PeerEntry.services` is display/bookkeeping only; the trust
        // gate never reads it. What actually bounds an introduction is that no `[services.*].allow`
        // entry names the subject's principals — which is why the `user_id` above must be PROVEN by
        // the subject rather than asserted by the endorser (a public user_id would otherwise
        // inherit its owner's grants).
        services: vec![],
        // Not a pairing write — no SAS happened, so no `paired_at` stamp and no pairing-proven
        // dial hint. Discovery resolves this peer.
        paired_at: None,
        // Only ever a user_id the SUBJECT proved, never one the endorser merely asserted.
        user_id: verified_user_id,
        last_addr: None,
    };
    let store = mesh.store.clone();
    blocking("join peer introduce", move || store.add(entry)).await??;

    // A trust-establishing write that involved NO human ceremony is exactly the one an operator
    // needs a record of — `pair`, `unpair` and `roster_install` all emit one, and this was the only
    // path that did not. #57's `principal` slot carries the ENDORSER, which is the question someone
    // reading this record will actually have.
    mesh.audit().record(crate::audit::AuditRecord::trust(
        now_ts(),
        "peer_introduce".into(),
        Some(nickname.clone()),
        Some(endorsed_by.clone()),
    ));
    tracing::info!(peer = %nickname, "installed peer from an endorsement (#65)");
    Ok(())
}

/// Handle a `peer_remove` control request: drop a paired
/// peer's authorization AND identity — the strict INVERSE of the pairing grant.
///
/// **Fail-safe teardown order (DECLARED).** The pairing grant writes, in order, (1) the
/// [`PeerEntry`] (identity — who the peer is) then (2) the config `allow` append (authorization —
/// what it may open). Removal is that grant's LIFO inverse: undo (2) FIRST via
/// `revoke_service_access` (strip the peer's stable principals from every `[services.*].allow`, the
/// security-relevant half), THEN undo (1) via [`PeerStore::remove`] (drop the identity row). This
/// leaves the peer MORE restricted, never less, at every partial-failure point:
///  - revoke fails → we abort BEFORE touching the store: the peer is unchanged (still fully
///    paired) — a clean, retriable failure, no half-state, and no orphaned config entry;
///  - revoke succeeds, store remove fails → the peer is known-but-forbidden (identity still
///    resolvable, but stripped from every allow → `select_service` denies it). Safe. Retriable:
///    both steps are idempotent, so re-running finishes the teardown.
///
/// (The alternative order — remove identity first — would, on a mid-failure, leave an ORPHAN
/// allow name that also trips the pairing collision guard on a later re-pair; revoke-first avoids
/// that.)
///
/// **Unknown nickname is an error (DECLARED).** When NEITHER half tears anything down (no allow
/// stripped, no PeerEntry deleted) the nickname matches no paired peer, and the removal FAILS with
/// a pointer at `mcpmesh status` — false success on a revocation surface would make a typo read
/// as a completed cut-off. Each half stays individually idempotent, so retrying a
/// partially-failed removal (allow stripped, identity row still present) still finishes clean.
///
/// **Live sessions.** Cut immediately, as of #54. The authorization half
/// ([`revoke_service_access`]) resolves the peer's devices and closes their live connections, so
/// an unpair no longer leaves in-flight mesh sessions running until the peer happens to
/// disconnect. Severing is connection-granular (see [`sever_principals`]).
///
/// **Status snapshot.** Not refreshed here — and it no longer needs to be: `status` reads the
/// config + store LIVE (control.rs `status_result`), so a revoke is reflected immediately even
/// though this detached handler holds no `DaemonState`. The functional truth is the store +
/// config (which the `pair --remove` tests assert on), and `status` now reads exactly that.
pub async fn remove_peer(state: &DaemonState, params: PeerRemoveParams) -> Result<()> {
    let mesh = state.mesh_required()?;
    let nickname = params.nickname;

    // (2)⁻¹ AUTHORIZATION: revoke first (the security-relevant half). Propagate its error so a
    // failure aborts before we touch the identity row (see the fail-safe reasoning above). Capture
    // whether an allow was actually stripped — one half of the actual-removal signal.
    let revoked = revoke_service_access(mesh, &nickname).await?;

    // BLOB hygiene (#38): strip the peer's stable principals from every blob scope too, BEFORE
    // dropping the identity row (we need the entries to compute the principals). Roster mode
    // only (the provider exists only there); a pure-pairing node is a no-op. The
    // last-device b64u rule mirrors the service-allow revoke.
    if let Some(provider) = mesh.app_blobs().await {
        let store = mesh.store.clone();
        let nick_r = nickname.clone();
        let principals: Vec<String> = blocking("join blob-revoke principals", move || {
            let (targets, others): (Vec<_>, Vec<_>) = store
                .list()?
                .into_iter()
                .partition(|e| e.nickname == nick_r);
            let mut principals = Vec::new();
            for t in &targets {
                principals.push(mcpmesh_net::EndpointId::from_bytes(t.endpoint_id).principal());
                if let Some(uid) = &t.user_id
                    && !others.iter().any(|o| o.user_id.as_deref() == Some(uid))
                    && !principals.contains(uid)
                {
                    principals.push(uid.clone());
                }
            }
            anyhow::Ok(principals)
        })
        .await??;
        if !principals.is_empty()
            && let Err(e) = provider.revoke_principals(&principals)
        {
            tracing::warn!(%e, "blob-scope revoke on unpair failed");
        }
    }

    // (1)⁻¹ IDENTITY: drop the PeerEntry (removes ALL entries sharing this nickname — nicknames are
    // not unique). redb writes block + fsync — run on a blocking thread. Capture
    // whether a PeerEntry was actually deleted — the other half of the actual-removal signal.
    let store = mesh.store.clone();
    let nickname_w = nickname.clone();
    let removed = blocking("join peer remove", move || store.remove(&nickname_w)).await??;

    // Actual-removal signal: neither an allow stripped NOR a PeerEntry deleted means the nickname
    // matches no paired peer. `pair --remove` is a REVOCATION surface — reporting success here
    // would let a typo ("alice" vs "Alice") read as a completed cut-off — so an all-no-op removal
    // is an ERROR, not a silent success. Retry-after-partial-failure still completes: with the
    // allow already stripped but the identity row still present, `removed` comes back true.
    if !revoked && !removed {
        anyhow::bail!("no paired peer named '{nickname}' — 'mcpmesh status' lists your peers");
    }

    tracing::info!(peer = %nickname, "unpaired peer");
    // Trust event: an unpair — reached only when something was ACTUALLY torn down (a
    // stripped allow OR a deleted PeerEntry; the all-no-op case errored above), so a refused
    // remove of a never-paired nickname writes NO phantom `unpair` record. Nickname only.
    mesh.audit().record(AuditRecord::trust(
        now_ts(),
        "unpair".into(),
        Some(nickname.clone()),
        // #57: deliberately NO principal — an unpair may tear down several devices under one
        // person, so there is no single subject to attribute.
        None,
    ));
    Ok(())
}

/// The vetted plan for a rename: the target [`PeerEntry`]s (the person). Post-#38 a rename is
/// a pure display mutation — the old nicknames are no longer needed (nothing authz-bearing
/// keyed on them), so the plan carries only the entries to re-nickname.
struct RenamePlan {
    targets: Vec<PeerEntry>,
}

/// Identify the person's entries and run the rename COLLISION GUARD (privilege-escalation defense,
/// mirroring pairing's `nickname_collision`). The person is every entry sharing `user_id` (renames all
/// their devices in one op), else the single entry named `nickname` (a provisional contact). Returns
/// `Ok(None)` when every target is already named `to` (a no-op), `Ok(Some(plan))` when the rename is
/// safe, or `Err` when no contact matches or `to` would inherit a DIFFERENT identity's access.
/// Blocking (redb + config read) — call on a blocking thread.
fn rename_plan(
    store: &PeerStore,
    user_id: Option<&str>,
    nickname: Option<&str>,
    to: &str,
) -> Result<Option<RenamePlan>> {
    let all = store.list()?;
    let targets: Vec<PeerEntry> = all
        .iter()
        .filter(|e| match user_id {
            Some(u) => e.user_id.as_deref() == Some(u),
            None => Some(e.nickname.as_str()) == nickname,
        })
        .cloned()
        .collect();
    if targets.is_empty() {
        anyhow::bail!("peer_rename: no matching contact");
    }
    if targets.iter().all(|e| e.nickname == to) {
        return Ok(None); // already named `to` — a no-op
    }

    let target_ids: std::collections::BTreeSet<[u8; 32]> =
        targets.iter().map(|e| e.endpoint_id).collect();
    // (a) display-uniqueness: a peer named `to` at an endpoint that is NOT one of the targets is
    // a DIFFERENT contact — a duplicate name would misdirect YOUR outbound dials
    // (`PeerStore::entry_for` is first-match by nickname) and make status ambiguous. Grants are
    // principal-keyed (#38) and unaffected by names; this guard protects routing/display only.
    if all
        .iter()
        .any(|e| e.nickname == to && !target_ids.contains(&e.endpoint_id))
    {
        anyhow::bail!("the nickname \"{to}\" is already used by another contact");
    }
    Ok(Some(RenamePlan { targets }))
}

/// Handle a `peer_rename` control request (the Contacts rename). Renames a contact's
/// display nickname — all the person's `PeerEntry`s to `to`. DISPLAY-ONLY (#38): grants are
/// principal-keyed, so no `allow` rewrite and no serving reload happen (and none are needed —
/// a rename can never change what a peer is granted). Guarded against duplicating another
/// contact's display name (outbound-dial routing is first-match by nickname). Held under
/// `reload_lock` so the guard and the store mutation stay one atomic critical section against
/// concurrent config/store writers.
pub async fn rename_peer(state: &DaemonState, params: PeerRenameParams) -> Result<()> {
    let mesh = state.mesh_required()?;
    let to = params.to.trim().to_string();
    if to.is_empty() {
        anyhow::bail!("peer_rename: the new nickname is empty");
    }
    let PeerRenameParams {
        user_id, nickname, ..
    } = params;
    if user_id.is_none() && nickname.is_none() {
        anyhow::bail!("peer_rename: no contact identified");
    }

    // Hold the whole guard→mutate→reload section under the SAME lock as grant/revoke/register, so a
    // concurrent config edit can neither race the collision guard nor clobber the allow rewrite.
    let _reload = mesh.reload_lock.lock().await;

    let store = mesh.store.clone();
    let (uid_c, pn_c, to_c) = (user_id.clone(), nickname.clone(), to.clone());
    let plan = blocking("join rename plan", move || {
        rename_plan(&store, uid_c.as_deref(), pn_c.as_deref(), &to_c)
    })
    .await??;
    let RenamePlan { targets } = match plan {
        Some(p) => p,
        None => return Ok(()), // no-op: already named `to`
    };

    // Mutate on a blocking thread: upsert each target `PeerEntry` (same endpoint_id, new
    // nickname). That is the WHOLE rename now (#38): grants are principal-keyed, so no config
    // rewrite and no serving reload — a rename is a pure display mutation with no serving blip
    // (the #38 fix: a rename can never desync a grant, because no grant names a nickname).
    let store = mesh.store.clone();
    let to_c = to.clone();
    blocking("join rename mutate", move || {
        for mut e in targets {
            e.nickname = to_c.clone();
            store.add(e)?;
        }
        anyhow::Ok(())
    })
    .await??;
    tracing::info!(to = %to, "renamed contact");
    Ok(())
}

/// The invite-time registration check: the refusal message when any requested service name has
/// no well-formed `[services.<name>]` entry, or `None` when every name is registered. Pure over
/// (requested, served) so the message shapes are unit-testable. The message states what IS
/// served (or that nothing is yet) and names the exact next command — never wire vocabulary.
fn unregistered_service_error(requested: &[String], served: &[String]) -> Option<String> {
    let unknown: Vec<&String> = requested.iter().filter(|r| !served.contains(r)).collect();
    let quoted: Vec<String> = unknown.iter().map(|n| format!("'{n}'")).collect();
    let named = match quoted.as_slice() {
        [] => return None,
        [one] => format!("no service named {one}"),
        many => format!("no services named {}", many.join(", ")),
    };
    Some(if served.is_empty() {
        format!(
            "{named} — nothing is served yet; register one with \
             'mcpmesh serve <name> -- <command>'"
        )
    } else {
        format!(
            "{named} — you serve: {} (see 'mcpmesh status')",
            served.join(", ")
        )
    })
}

/// Mint a one-time pairing invite granting `services`.
///
/// Builds an [`Invite`] { 32 CSPRNG-byte secret, our endpoint id + dialable address, our
/// suggested nickname, the granted services, a `≤ now + 24h` expiry }, registers it in the
/// live registry so the accept loop's `mcpmesh/pair/1` branch will redeem it, and returns the
/// copyable `mcpmesh-invite:` line. Logs a trust event carrying NO secret and NO peer
/// id — an invite has no peer yet; the redeemer is only known once it dials the rendezvous.
///
/// The secret uses the OS CSPRNG via `rand::rngs::OsRng` — the SAME source the device-key
/// mint uses (mcpmesh-trust), no new crate. The address comes from `endpoint.addr()`; we first
/// wait (bounded by [`RELAY_READY_TIMEOUT`]) for the endpoint to come online so the addr
/// carries the home-relay URL the redeemer bootstraps from across NAT.
///
/// **Registration check (DECLARED).** Every requested name must have a well-formed
/// `[services.<name>]` entry, or the mint is REFUSED: an invite for an unregistered name would
/// redeem fine, pass the safety-code ceremony, and only fail at connect time on the REDEEMER's
/// machine — the worst place to discover the inviter's typo. Validated against the SAME view
/// `status` renders ([`service_infos`], read live from disk like `status_result`), so the
/// refusal's "you serve:" list always matches what `mcpmesh status` shows.
/// Validate a caller-supplied LOCAL alias (#87) — the one shared rule for both directions.
///
/// The same checks `set_nickname` applies, because the value lands in the same place: a stored
/// `PeerEntry.nickname`, which is the `<peer>/<service>` mount prefix the porcelain splits on. The
/// aliases shipped unvalidated in the first draft, and the review found `"alice/notes"` made the
/// peer permanently unmountable (`split_target` cuts at the first `/`) while `" alice "` slipped
/// past the collision check — which compares exact bytes — to render identically to an existing
/// `alice` in any trimming UI. Two peers, one display name, which is the whole invariant.
///
/// Returns the TRIMMED value. Empty after trimming is an error rather than `None`: a UI passing
/// through an untouched field must not silently get the name the user was avoiding.
fn validated_alias(field: &str, alias: Option<String>) -> Result<Option<String>> {
    let Some(raw) = alias else { return Ok(None) };
    let name = raw.trim().to_string();
    if name.is_empty() {
        anyhow::bail!(crate::control::InvalidParams(format!(
            "{field} must not be empty (omit it to use the name the peer suggests)"
        )));
    }
    if name.contains('/') {
        anyhow::bail!(crate::control::InvalidParams(format!(
            "{field} must not contain '/': the nickname is the <peer>/<service> mount prefix, so \
             one would make every mount of that peer unparseable"
        )));
    }
    if name.chars().any(char::is_control) {
        anyhow::bail!(crate::control::InvalidParams(format!(
            "{field} must not contain control characters"
        )));
    }
    if name.chars().count() > MAX_ALIAS_CHARS {
        anyhow::bail!(crate::control::InvalidParams(format!(
            "{field} is {} characters; the limit is {MAX_ALIAS_CHARS}",
            name.chars().count()
        )));
    }
    Ok(Some(name))
}

/// Cap on a local alias (#87). A display name, not a document — and `peer_nickname` is persisted
/// into the whole-file-rewritten invite store, so an unbounded one is a write amplifier.
const MAX_ALIAS_CHARS: usize = 64;

pub(crate) async fn mint_invite(
    services: Vec<String>,
    app_label: Option<String>,
    max_uses: Option<u32>,
    peer_nickname: Option<String>,
    // #86: mint a SELF-ENROLLMENT invite — the redeemer becomes another device of this person.
    as_self: bool,
    mesh: &MeshState,
) -> Result<InviteResult> {
    use rand::RngCore;

    // #87: `None` = 1, the single-use default every existing caller already gets. `0` is REJECTED
    // rather than silently meaning "unusable" — a caller asking for zero redemptions has a bug,
    // and answering with an invite nobody can redeem hides it. Above the cap is CLAMPED, and the
    // clamped value is what comes back, so a caller is never told it got more than it did.
    let uses_remaining = match max_uses {
        None => 1,
        Some(0) => anyhow::bail!(crate::control::InvalidParams(
            "max_uses must be at least 1 (omit it for a single-use invite)".into()
        )),
        Some(n) => n.min(mcpmesh_local_api::MAX_INVITE_USES),
    };

    // #87: OUR local name for whoever redeems. Validated here so a bad one is a clean -32602
    // rather than a ceremony that fails halfway through, on the other machine, minutes later.
    // #86: a self-enrollment invite mints IDENTITY, not access. A multi-use one is a standing
    // offer to become this person, so it is refused at mint — the same call `peer_nickname` makes,
    // for a much higher-value payload.
    if as_self && max_uses.is_some_and(|n| n > 1) {
        anyhow::bail!(crate::control::InvalidParams(format!(
            "as_self cannot be combined with max_uses = {}: a multi-use SELF-ENROLLMENT invite is \
             a standing offer to become this person. Mint one per device.",
            max_uses.unwrap_or(1)
        )));
    }
    // A self-invite grants nothing — the two devices are the same person, not peers — so a
    // services list on one is a caller error rather than something to silently ignore.
    if as_self && !services.is_empty() {
        anyhow::bail!(crate::control::InvalidParams(
            "as_self grants nothing: your own devices are not peers of each other. Omit services."
                .into()
        ));
    }
    if as_self {
        // #214: an ENROLLED device cannot enroll a third — `inviter_ctx`'s `sign_binding` returns
        // `None` for it — but nothing said so at mint. The refusal surfaced one round trip later,
        // on the OTHER machine, as the opaque -32049 ("that invite didn't work"), which is
        // dead-end advice for a permanent condition. Refused HERE, coded, on the device that can
        // act on it. Checked BEFORE the no-identity case below, and it has to be: an enrolled
        // device's `self_binding()` is `Some` (the adopted one), so that check cannot see it —
        // this is the only one that names the condition.
        if mesh
            .adopted_binding
            .read()
            .expect("adopted_binding lock not poisoned")
            .is_some()
        {
            anyhow::bail!(crate::pairing::rendezvous::PairRefusal::new(
                mcpmesh_local_api::ERR_SELF_ENROLL_NO_KEY,
                "this device was enrolled into another device's identity and holds no user key to \
                 enroll with. Enroll from the device that holds the key, or detach this one first \
                 if it should not be enrolled at all",
            ));
        }
        // No user key at all means there is no identity to enroll INTO. The redeem arm refuses
        // this too, but on the other machine; a mint that cannot be redeemed is refused here.
        if mesh.self_binding().is_none() {
            anyhow::bail!("this device has no user identity to enroll into");
        }
    }
    let peer_nickname = validated_alias("peer_nickname", peer_nickname)?;
    // One alias applied to EVERY redeemer of a multi-use invite collides on the second redemption.
    // Refused at mint rather than producing an invite that works exactly once. Reports the value
    // the CALLER SENT, not the clamped one — "max_uses = 64" for a request of 10_000 names a number
    // they never wrote.
    if peer_nickname.is_some()
        && let Some(requested) = max_uses.filter(|n| *n > 1)
    {
        anyhow::bail!(crate::control::InvalidParams(format!(
            "peer_nickname cannot be combined with max_uses = {requested}: one local name applied \
             to every redeemer would collide on the second redemption. Mint separate single-use \
             invites, or omit peer_nickname and rename afterwards with peer_rename"
        )));
    }
    // #87 gate: catch a collision the operator can still fix, HERE, where the error reaches the
    // person who chose the name. At redemption it can only be answered opaquely — the redeemer must
    // not learn our private name for it — so a mint-time check is the only place this is
    // actionable. Not a guarantee: a peer added after minting can still collide later.
    if let Some(alias) = &peer_nickname {
        let store = mesh.store.clone();
        let alias_c = alias.clone();
        let taken = blocking("join alias collision check", move || {
            anyhow::Ok(store.list()?.into_iter().any(|e| e.nickname == alias_c))
        })
        .await??;
        if taken {
            anyhow::bail!(crate::control::InvalidParams(format!(
                "peer_nickname '{alias}' is already the name of a peer you have paired with — \
                 pick another, or rename that peer first with peer_rename"
            )));
        }
    }

    // The opaque app label (#31) is capped: the invite line is a human-copied base32 artifact,
    // so a caller cannot bloat it. mcpmesh never interprets the label — this bounds size only.
    if let Some(label) = &app_label
        && label.len() > crate::pairing::MAX_APP_LABEL_LEN
    {
        anyhow::bail!(
            "app_label is {} bytes; the maximum is {}",
            label.len(),
            crate::pairing::MAX_APP_LABEL_LEN
        );
    }

    // An invite that grants nothing is useless, and a silently-empty list is exactly the
    // symptom of a param typo like `{service: "kb"}` (singular) slipping past validation
    // (#34). Reject it before minting, matching the CLI porcelain which already makes the
    // service arg required. `deny_unknown_fields` on `InviteParams` catches the typo at parse
    // time; this is the belt-and-braces guard on the value itself.
    //
    // #86: NOT for a self-enrollment invite. There, granting nothing is the whole point — your own
    // devices are not peers of each other — so the "useless" reasoning inverts.
    if services.is_empty() && !as_self {
        anyhow::bail!(
            "invite must name at least one registered service (an invite granting nothing is useless)"
        );
    }

    // Registration check FIRST — before the CSPRNG mint and the online()-wait, so a typo'd
    // name fails fast and never touches the invite registry.
    let cfg = Config::load(&mesh.config_path)
        .map_err(|e| anyhow::anyhow!("config error in {}: {e}", mesh.config_path.display()))?;
    // Served names include EPHEMERAL registrations (#36) — an invite may grant an ephemeral
    // service just like a persistent one.
    let ephemeral = mesh
        .ephemeral_services
        .lock()
        .expect("ephemeral_services lock not poisoned")
        .clone();
    // #100: the KNOWN-names view, not the live-registry one. An invite is redeemed later, after
    // reloads, so a service the operator has just added to `config.toml` must still mint.
    let served: Vec<String> = crate::daemon::known_service_names(&cfg, &ephemeral);
    if let Some(msg) = unregistered_service_error(&services, &served) {
        anyhow::bail!(msg);
    }

    // 32 CSPRNG bytes — the single-use bearer credential.
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);

    let inviter_id = *mesh.endpoint.id().as_bytes();

    // Our own dialable address, WITH the relay URL when we can get it: `online()` completes on
    // a home-relay handshake, after which `addr()` carries that relay. Bounded so a relay-less
    // (localhost/test) endpoint still mints promptly with its direct addrs.
    let _ = tokio::time::timeout(RELAY_READY_TIMEOUT, mesh.endpoint.online()).await;
    let inviter_addr_json = serde_json::to_string(&mesh.endpoint.addr())
        .context("serialize our own endpoint address for the invite")?;

    let now = epoch_now_u64();
    let expires_at_epoch = now + INVITE_TTL.as_secs();
    let invite = Invite {
        as_self,
        peer_nickname,
        secret,
        inviter_id,
        inviter_addr_json,
        nickname: mesh.self_nickname(),
        services: services.clone(),
        expires_at_epoch,
        app_label,
        uses_remaining,
    };
    let invite_line = invite.encode();
    // Reap expired invites before minting so a long-lived daemon's registry can't grow
    // unboundedly with never-redeemed invites (bounds map growth; the invite lifetime cap,
    // the invite-lifetime cap). Cheap: one lock + retain over a small map.
    mesh.invites.remove_expired(now).await;
    // #87b: persist BEFORE handing the invite out. A mint that cannot be written must fail — the
    // 24h TTL on the line we are about to return is a promise, and issuing one we already know
    // will not survive the next restart is precisely what #87 filed.
    mesh.invites.mint(invite).await.context(
        "persist the outstanding invite (its advertised TTL depends on surviving a restart)",
    )?;

    // Trust event: record the mint. NO secret, NO peer id (there is no peer yet).
    tracing::info!(?services, uses_remaining, "invite minted");
    Ok(InviteResult {
        invite_line,
        expires_at_epoch,
        uses_remaining,
    })
}

/// Handle a `pair` control request: dial the inviter named by
/// `invite_line` on `mcpmesh/pair/1`, verify its TLS identity binds the invite's `inviter_id`
/// (the address-swap defense), prove the secret, write OUR dial-back [`PeerEntry`], and return
/// the inviter's nickname + the display-only SAS. Delegates to
/// [`crate::pairing::rendezvous::redeem_invite`], threading our own endpoint + self-nickname +
/// store. The inviter-side authorization (adding US to its service `allow`) happens on ITS
/// daemon inside its rendezvous handler — see [`grant_service_access`].
///
/// `self_enroll` carries the caller's #178 consent: a `mcpmesh-enroll:` line is refused before any
/// dial unless the caller asked for that ceremony.
pub(crate) async fn redeem(
    state: &DaemonState,
    invite_line: String,
    as_nickname: Option<String>,
    self_enroll: crate::pairing::SelfEnroll,
) -> Result<PairResult> {
    let mesh = state.mesh_required()?;
    // #87: validated HERE, at the control seam, so a blank field is a clean -32602 rather than a
    // ceremony that dials a stranger and fails partway. Empty is rejected rather than treated as
    // absent: `as_nickname: ""` almost certainly means a UI passed through an untouched field, and
    // silently falling back to the invite's suggestion is how a user ends up with the very name
    // they were trying to avoid.
    let as_nickname = validated_alias("as_nickname", as_nickname)?;
    // #43: the redeemer-side MUTUAL grant hook — grant the inviter access to ALL services this
    // node serves (the same stable-principal + reload discipline as the inviter-side grant).
    let grant_mesh = mesh.clone();
    let grant_back: crate::pairing::rendezvous::GrantBackFn =
        Box::new(move |principal, display| {
            let mesh = grant_mesh.clone();
            Box::pin(async move {
                let served: Vec<String> = match Config::load(&mesh.config_path) {
                    Ok(cfg) => cfg.services.keys().cloned().collect(),
                    Err(e) => {
                        // A config we can't read means we can't know what we serve; the mutual
                        // grant is best-effort (the pairing itself already succeeded), so log
                        // and skip rather than fail the ceremony.
                        tracing::warn!(%e, "mutual grant-back skipped: config unreadable");
                        return Ok(());
                    }
                };
                if served.is_empty() {
                    return Ok(()); // we serve nothing → nothing to grant back
                }
                // BEST-EFFORT (#43): the pairing (store write + inviter-side grant) already
                // succeeded and the one-time invite is burned, so a grant-back failure must NOT
                // fail the ceremony (which would strand the user in a paired-but-errored state
                // with no invite to retry). Log it; the operator can re-grant via
                // `service_allow_grant`. The reload_lock inside serializes it safely.
                if let Err(e) = grant_service_access(&mesh, &principal, &display, &served).await {
                    tracing::warn!(%e, "mutual grant-back failed (pairing still succeeded)");
                }
                Ok(())
            })
        });
    crate::pairing::rendezvous::redeem_invite(
        mesh.endpoint.clone(),
        mesh.self_nickname(),
        invite_line,
        as_nickname,
        self_enroll,
        // #86: persist an adopted self-enrollment binding — the only copy there will ever be.
        Some(adopt_hook(mesh)),
        mesh.store.clone(),
        mesh.self_binding(),
        Some(grant_back),
    )
    .await
}

/// Handle a `peer_services` control request (#52): resolve `peer` to its endpoint, probe it
/// over `mcpmesh/ping/1`, and return the services its pong reports the caller is admitted to.
/// Only the caller's own admitted services. Reuses a cache entry younger than `REACH_TTL_SECS`
/// rather than always probing (#89) — see `probe_peer_cached` for why an unconditional probe made
/// this verb collide with the ping rate limiter and report healthy peers as offline.
pub(crate) async fn peer_services(
    state: &DaemonState,
    peer: String,
) -> Result<mcpmesh_local_api::PeerServicesResult> {
    let mesh = state.mesh_required()?;
    let endpoint_id = resolve_peer_endpoint(mesh, &peer).await?;
    // #223: this verb DIALS (a probe), so it asks the full outbound predicate — roster-revoked
    // endpoints and roster devices under a revoked `b64u:` identity too. Checked HERE rather than
    // in the shared resolver: `peer_diagnostics` and `peer_hint_clear` share it and dial nothing,
    // and refusing a roster-revoked device there blocked legitimate operator actions. Before the
    // cached probe, since a fresh cache entry would otherwise answer without any dial check.
    let m = mesh.clone();
    anyhow::ensure!(
        !blocking("join peer_services revocation check", move || {
            crate::daemon::dial::dial_refused(&m, &endpoint_id)
        })
        .await?,
        "peer '{peer}' is REVOKED on this node"
    );
    let entry = crate::daemon::reach::probe_peer_cached(mesh, endpoint_id).await;
    anyhow::ensure!(
        entry.reachable,
        "peer '{peer}' is unreachable — cannot fetch its shared services"
    );
    // #215: a reachable row with no pong holds no services at all — a session's path watcher wrote
    // it, and the probe that should have filled it was refused by the peer's limiter or fetched
    // nothing. Its empty list is "not asked", and answering it would tell the caller the peer
    // offers nothing. Refuse, retryably, instead.
    anyhow::ensure!(
        entry.pong_at.is_some(),
        "peer '{peer}' is reachable, but its shared services could not be fetched just now — retry"
    );
    Ok(mcpmesh_local_api::PeerServicesResult {
        services: entry.services,
    })
}

/// Dump the DURABLE per-peer state for one peer (#140), plus this node's live view of it.
///
/// How long [`peer_diagnostics`] waits on iroh's remote-map actor before reporting no entry.
///
/// `Endpoint::remote_info` awaits an mpsc send and a oneshot reply with no timeout of its own. Every
/// other await in that handler is bounded or a cache read, and a diagnostic reached for when
/// something is already wrong is the last thing that should hang.
const REMOTE_INFO_TIMEOUT: Duration = Duration::from_secs(2);

/// Label ONE transport address for a diagnostic capture (#140): IPs verbatim, relay URLs as
/// `relay <url>` SANITIZED to scheme+host+port.
///
/// Shared by the stored hint and by iroh's own view, deliberately. The two lists are read side by
/// side and diffed against each other, so labelling them differently would both make a formatting
/// difference look like a real one and let one list leak a userinfo token the other strips.
pub(crate) fn label_transport_addr(a: &iroh::TransportAddr) -> String {
    match a {
        iroh::TransportAddr::Ip(s) => s.to_string(),
        iroh::TransportAddr::Relay(u) => format!("relay {}", crate::daemon::sanitize_relay_url(u)),
        // `TransportAddr` is `#[non_exhaustive]`. A `{other:?}` fallback was the first cut and it
        // quietly voids the guarantee two lines up: today the only other variant is `Custom`, whose
        // Debug is an opaque transport id and bytes, but a future URL-bearing variant would print
        // unsanitized into output whose entire purpose is being pasted into a public issue. The
        // variant is named, its contents are not.
        _ => "(unsupported transport address)".to_string(),
    }
}

/// The two set differences between our stored hint and iroh's view (#140) — PURE, so the direction
/// of each is pinned by a test rather than by reading the call site.
///
/// Returned as `(in_hint_only, in_iroh_only)`. Getting these backwards is the whole failure mode:
/// the fields are adjacent, same-typed, and a reversed pair reads perfectly plausibly while telling
/// a reporter the exact opposite of what is happening on their machine.
///
/// An INFERENCE, not a reading: iroh 1.0.3's `TransportAddrInfo` carries no provenance, so this
/// cannot say an address *came from* the hint — only that it is or is not in both lists.
pub(crate) fn addr_differences(hint: &[String], known: &[String]) -> (Vec<String>, Vec<String>) {
    let hint_set: std::collections::HashSet<&str> = hint.iter().map(String::as_str).collect();
    let known_set: std::collections::HashSet<&str> = known.iter().map(String::as_str).collect();
    (
        hint.iter()
            .filter(|a| !known_set.contains(a.as_str()))
            .cloned()
            .collect(),
        known
            .iter()
            .filter(|a| !hint_set.contains(a.as_str()))
            .cloned()
            .collect(),
    )
}

/// The question behind #140 is "what does a long-lived pairing carry that a fresh identity does
/// not, that could durably prevent a hole-punch while leaving relayed connectivity healthy?"
///
/// Scoped honestly: the only durable per-peer state ON THIS NODE'S DISK that the DIAL PATH reads is
/// [`PeerEntry::last_addr`](crate::allowlist::PeerEntry::last_addr). Other durable state exists and
/// is not this — a discovery record published under the same long-lived key, accumulated
/// `services`, legacy allow entries, `identity_conflict_epoch` (#134) — but none of it feeds the
/// dial. That makes the hint the first thing to compare, not the proven cause.
///
/// So this reports the hint verbatim, whether it PARSES and matches this peer (an unusable hint is
/// silently discarded at every dial, which is invisible from outside), the addresses inside it, and
/// the live reachability row — one capture, both sides of the question, runnable on both ends of a
/// stuck pairing.
///
/// **Deliberately carries transport vocabulary**, alone among the verbs. See
/// [`PeerDiagnosticsResult`](mcpmesh_local_api::PeerDiagnosticsResult).
///
/// Read-only, and it has to be: it reads the reachability CACHE rather than `status`'s projection,
/// which would spawn a background probe for every stale peer. A diagnostic that dials the thing it
/// is measuring is a participant in the reproduction, not an observer of it (#140 gate).
/// Since `api_minor` 56 it also reports IROH's own view of the peer's addresses, which is the other
/// half of the same question: the fields above say what this node stored, `known_addrs` says what
/// iroh made of it.
///
pub(crate) async fn peer_diagnostics(
    state: &DaemonState,
    peer: &str,
) -> Result<mcpmesh_local_api::PeerDiagnosticsResult> {
    let mesh = state.mesh_required()?;
    let endpoint_id = resolve_peer_endpoint(mesh, peer).await?;
    let store = mesh.store.clone();
    let entry = blocking("join peer-diagnostics store read", move || {
        store.resolve(&endpoint_id)
    })
    .await??
    .with_context(|| format!("peer '{peer}' is not in the allowlist"))?;

    let id = iroh::EndpointId::from_bytes(&endpoint_id)
        .map_err(|e| anyhow::anyhow!("stored endpoint id for '{peer}' is invalid: {e}"))?;
    // Parse the hint the same way the DIAL does — via `stored_dial_addr`, not a bespoke reading —
    // so "usable" here means usable to the code that actually dials, and the two cannot drift.
    let dialed = crate::daemon::dial::stored_dial_addr(entry.last_addr.as_deref(), id);
    // EVERY address the hint carries, IP and relay alike, each labelled.
    //
    // Filtering to IP hid the one shape most worth seeing (#140 gate): an invite made while only
    // the inviter's relay path was up stores a RELAY-only hint, which is exactly the state #124
    // identified as harmful — it can never punch — and it rendered as an empty line. Relay URLs are
    // SANITIZED to scheme+host+port through the same helper `status` uses: an operator-supplied
    // relay URL can carry a userinfo token, and this output is meant to be pasted into an issue.
    let hint_addrs: Vec<String> = dialed.addrs.iter().map(label_transport_addr).collect();
    // An id-only `EndpointAddr` is what a MISSING or REJECTED hint degrades to, so a stored hint
    // that yields no addresses is one being thrown away at every dial.
    let hint_usable = entry.last_addr.is_some() && !dialed.addrs.is_empty();

    // The LIVE row, read straight out of the cache — deliberately NOT via `reachability_of`.
    //
    // That helper spawns a background probe for every peer whose entry is stale or missing, so
    // calling it here would make this "read-only" verb dial EVERY paired peer, write both caches,
    // push `Reachability` frames at any subscriber, and spend the peer's #89 ping budget. On a
    // freshly restarted daemon that is one dial per peer. A diagnostic used ON a live reproduction
    // must not be a participant in it (#140 gate).
    //
    // Keyed on the ENDPOINT ID, not the nickname: nicknames collide (which is the whole reason
    // #41/#42/#73 exist), and joining this peer's durable state to a namesake's live row is the
    // most confusing possible output for a capture whose entire job is comparison.
    let reachability = {
        let cache = mesh
            .reachability
            .lock()
            .expect("reachability lock not poisoned");
        cache.get(&endpoint_id).map(|e| {
            let age = (crate::util::epoch_now_i64() - e.probed_at).max(0);
            crate::daemon::reach::reachability_row(
                entry.nickname.clone(),
                endpoint_id,
                Some(e),
                Some(age as u64),
            )
        })
    };

    // IROH's own view (#140, api_minor 56) — a point read of the remote map. `remote_info` does
    // not dial, probe, or trigger an address lookup, which is what keeps this verb safe to run ON
    // the live reproduction it is meant to observe.
    //
    // `None` is preserved rather than flattened to an empty list. It does NOT mean "never heard of":
    // iroh reaps a remote's state ~60s after its last connection closes, so absent is the normal
    // answer both for a peer never dialled and for one talked to minutes ago. It still means
    // something different from "iroh has an entry and holds no address in it".
    // Bounded: `remote_info` awaits an mpsc send plus a oneshot with no timeout of its own, while
    // every other await in this handler is bounded or cache-local. A diagnostic must not be the
    // thing that hangs. A timeout degrades to "no entry", which is already a modelled state.
    let known_addrs: Option<Vec<mcpmesh_local_api::KnownAddr>> =
        tokio::time::timeout(REMOTE_INFO_TIMEOUT, mesh.endpoint.remote_info(id))
            .await
            .ok()
            .flatten()
            .map(|info| {
                info.addrs()
                    .map(|a| mcpmesh_local_api::KnownAddr {
                        addr: label_transport_addr(a.addr()),
                        active: matches!(a.usage(), iroh::endpoint::TransportAddrUsage::Active),
                    })
                    .collect()
            });
    // The differences are only meaningful against a view that EXISTS.
    //
    // Flattening `None` to an empty list here — which the first cut did — makes every hint address
    // "unknown to iroh" whenever iroh has no entry, which is the DEFAULT state of a daemon that has
    // not dialled the peer yet, including the freshly auto-started one the CLI spawns. It printed
    // "stale: 192.168.1.50:4433" about a hint that was current and correct. A diagnostic that
    // confidently mislabels the common case is worse than no diagnostic (#140 gate).
    let (hint_addrs_unknown_to_iroh, iroh_addrs_not_in_hint) = match &known_addrs {
        Some(known) => {
            let labels: Vec<String> = known.iter().map(|k| k.addr.clone()).collect();
            addr_differences(&hint_addrs, &labels)
        }
        None => (Vec::new(), Vec::new()),
    };

    Ok(mcpmesh_local_api::PeerDiagnosticsResult {
        nickname: entry.nickname,
        principal: mcpmesh_net::EndpointId::from_bytes(entry.endpoint_id).principal(),
        user_id: entry.user_id,
        paired_at: entry.paired_at,
        last_addr: entry.last_addr,
        hint_addrs,
        hint_usable,
        reachability,
        known_addrs,
        hint_addrs_unknown_to_iroh,
        iroh_addrs_not_in_hint,
    })
}

/// Forget one peer's persisted dial hint (#140).
///
/// The ONE piece of durable per-peer state on this node's disk that the dial path reads, and the
/// only thing a long-lived pairing carries that a freshly paired identity does not. Clearing it
/// makes the pairing addressing-equivalent to a fresh one, which is the exact difference #140 is
/// about.
///
/// Advisory, never authorization: the row, its `user_id`, its services and its pairing stamp are
/// untouched, and an absent hint is a supported state (`stored_dial_addr` degrades to an id-only
/// dial). Nothing clears a hint automatically — see the verb's rustdoc for why not.
///
/// Audited: it changes how this node dials a peer, and #140 is a two-machine debugging session where
/// "what did we do, and when" is the record.
pub(crate) async fn peer_hint_clear(
    state: &DaemonState,
    peer: &str,
) -> Result<mcpmesh_local_api::PeerHintClearResult> {
    let mesh = state.mesh_required()?;
    // EVERY device the selector names, not just the first (#140 gate).
    //
    // `resolve_peer_endpoint` returns ONE endpoint, and for a `b64u:` user_id that is whichever
    // device happens to sort first in redb's key order. Since #186 the racing dial path attaches the
    // stored hint for EVERY device of a person, so clearing one and reporting success would leave
    // the pairing still dialling from stored state while telling the operator it addresses like a
    // fresh identity. On the two-device fleet in #140 that is the difference between the experiment
    // working and quietly proving nothing.
    let targets = peer_hint_targets(mesh, peer).await?;
    let store = mesh.store.clone();
    let ids = targets.clone();
    // Paired with its device, so the audit can name WHICH device changed rather than guessing.
    let outcomes: Vec<([u8; 32], Option<String>)> =
        blocking("join peer-hint-clear store write", move || {
            ids.into_iter()
                .map(|id| store.clear_last_addr(&id).map(|f| (id, f)))
                .collect::<Result<Vec<_>>>()
        })
        .await??;

    let mut forgotten = Vec::new();
    for (id, previous) in outcomes {
        let Some(previous) = previous else {
            continue; // this device had no hint: nothing changed, nothing to record
        };
        // One record per device ACTUALLY changed. A person with three devices of which one had a
        // hint must not read as three clears in a two-machine debugging capture.
        mesh.audit().record(mcpmesh_local_api::AuditRecord::trust(
            crate::audit::record::now_ts(),
            "peer_hint_clear".into(),
            // Surface-clean: the resolved `eid:` principal, never the caller-typed selector, so
            // a nickname collision cannot make two peers indistinguishable in the record.
            Some(mcpmesh_net::EndpointId::from_bytes(id).principal()),
            None,
        ));
        forgotten.push(previous);
    }

    Ok(mcpmesh_local_api::PeerHintClearResult {
        cleared: forgotten.len(),
        forgotten,
    })
}

/// Every stored device a `peer_hint_clear` selector names — and an ERROR if it names none.
///
/// Split out because `resolve_peer_endpoint` cannot serve this verb on either count:
///
/// - Its `eid:` arm decodes a principal and returns it **without consulting the store**, so an
///   unpaired `eid:` would flow through to a `cleared: 0` that is indistinguishable from a known
///   peer with no hint. An operator running an experiment must not be told they cleared a hint for
///   a device this node has never paired with (#140 gate — the first version did exactly that,
///   while four doc sites claimed it errored).
/// - It returns ONE endpoint for a `b64u:` user_id.
async fn peer_hint_targets(mesh: &Arc<MeshState>, peer: &str) -> Result<Vec<[u8; 32]>> {
    let store = mesh.store.clone();
    if let Some(user_id) = peer.strip_prefix("b64u:") {
        let uid = format!("b64u:{user_id}");
        let entries = blocking("join peer-hint-targets user lookup", move || {
            store.entries_for_user(&uid)
        })
        .await??;
        if entries.is_empty() {
            anyhow::bail!("no paired device for user '{peer}'");
        }
        return Ok(entries.into_iter().map(|e| e.endpoint_id).collect());
    }
    // Nickname and `eid:` both resolve to one device — but only the nickname path proves the peer
    // is stored, so membership is checked here for both.
    let id = resolve_peer_endpoint(mesh, peer).await?;
    let store = mesh.store.clone();
    let known = blocking("join peer-hint-targets membership", move || {
        store.resolve(&id)
    })
    .await??
    .is_some();
    if !known {
        anyhow::bail!("no paired peer '{peer}' — nothing to clear");
    }
    Ok(vec![id])
}

/// Resolve a `peer` selector to a stored endpoint id (#52): an `eid:<hex>` decodes directly;
/// else a stored `PeerEntry` by nickname; else the first device under a `b64u:` user_id.
pub(crate) async fn resolve_peer_endpoint(mesh: &Arc<MeshState>, peer: &str) -> Result<[u8; 32]> {
    if let Some(hex) = peer.strip_prefix("eid:") {
        let bytes = data_encoding::HEXLOWER
            .decode(hex.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid eid principal: not lowercase hex"))?;
        let eid: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid eid principal: expected 32 bytes"))?;
        // The `eid:` arm returns EARLY, so the revocation check below never saw it — caught by the
        // test for this guard, not by review. A direct device principal is the most precise way to
        // reach a revoked machine, so of the three selectors it is the one that most needed it.
        anyhow::ensure!(
            !store_refused(mesh, eid).await?,
            "peer '{peer}' is REVOKED on this node"
        );
        return Ok(eid);
    }
    let store = mesh.store.clone();
    let peer_owned = peer.to_string();
    let eid = tokio::task::spawn_blocking(move || -> Result<Option<[u8; 32]>> {
        if let Some(e) = store.entry_for(&peer_owned)? {
            return Ok(Some(e.endpoint_id));
        }
        Ok(store
            .entries_for_user(&peer_owned)?
            .first()
            .map(|e| e.endpoint_id))
    })
    .await
    .context("join peer resolve for peer_services")??;
    let eid = eid
        .with_context(|| format!("no paired peer '{peer}' — 'mcpmesh status' lists your peers"))?;
    // #85 ask 4: this feeds `peer_services` (which DIALS), `peer_diagnostics` and `peer_hint_clear`
    // (which do not — see `peer_diagnostics_never_dials`). The store revocation check here is #85's
    // and predates the split; the full outbound predicate (#223) is asked by `peer_services` alone,
    // because it is the only one of the three that reaches the device.
    anyhow::ensure!(
        !store_refused(mesh, eid).await?,
        "peer '{peer}' is REVOKED on this node"
    );
    Ok(eid)
}

/// `PeerStore::is_refused` on the blocking pool — redb reads never run on a runtime worker.
async fn store_refused(mesh: &Arc<MeshState>, eid: [u8; 32]) -> Result<bool> {
    let store = mesh.store.clone();
    crate::util::blocking("join peer revocation check", move || store.is_refused(&eid)).await
}

/// Handle an `unregister_service` control request/// Handle an `unregister_service` control request (#50): remove the whole `[services.<name>]`
/// entry (allow list included) AND drop any in-memory ephemeral registration of that name,
/// then hot-reload so the running registry stops serving it. Idempotent (unknown name → clean
/// no-op), serialized under the SAME `reload_lock` as `register_service`/#44. In-flight
/// sessions finish (the reload rebuilds the registry without it; no NEW sessions admitted).
pub(crate) async fn unregister_service(state: &DaemonState, name: String) -> Result<()> {
    let mesh = state.mesh_required()?;
    let _reload = mesh.reload_lock.lock().await;

    // Drop an in-memory ephemeral registration of this name (if any).
    let dropped_ephemeral = mesh
        .ephemeral_services
        .lock()
        .expect("ephemeral_services lock not poisoned")
        .remove(&name)
        .is_some();

    // Remove the persistent config entry (if any).
    let config_path = mesh.config_path.clone();
    let name_w = name.clone();
    let removed_config = blocking("join unregister config write", move || {
        remove_service_from_config(&config_path, &name_w)
    })
    .await??;

    // Reload only if something actually changed (else the running registry already excludes it).
    if dropped_ephemeral || removed_config {
        reload_services_from_disk(mesh, "unregister").await?;
    }
    tracing::info!(service = %name, dropped_ephemeral, removed_config, "unregistered service");
    Ok(())
}

/// Set this node's CUSTOM relay set LIVE (#53, the `set_relays` verb). `relay_urls` is the
/// DESIRED custom set; the daemon computes a diff against the currently-persisted set and, when
/// the node is already in `relay_mode = "custom"`, applies the delta to the RUNNING endpoint via
/// iroh 1.0.3 `Endpoint::insert_relay`/`remove_relay` — no endpoint rebuild, no dropped sessions
/// — then persists `[network] relay_mode="custom" relay_urls=[…]`. Serialized under the SAME
/// `reload_lock` as every other config mutator.
///
/// - **Validation is atomic and up front:** an empty list is rejected (custom mode requires ≥1
///   relay; fully disabling relays is a `relay_mode="disabled"` restart, not this verb), and every
///   URL must parse as an iroh `RelayUrl` — a single bad entry aborts with NOTHING applied.
/// - **Idempotent:** if the desired set (order-independent) equals the persisted set, no writes
///   and no endpoint calls happen (`changed = false`).
/// - **Mode transitions are NOT live:** iroh cannot swap a running endpoint's relay MODE
///   (`default`'s built-in map / a `disabled` no-relay endpoint). So when the node's current mode
///   is not `custom`, the new set is PERSISTED but not applied live and `restart_required = true`
///   is returned. On the custom→custom path `restart_required = false` (already live).
pub(crate) async fn set_relays(
    state: &DaemonState,
    relay_urls: Vec<String>,
) -> Result<SetRelaysResult> {
    let mesh = state.mesh_required()?;

    // Validate atomically, BEFORE the lock and before any mutation: non-empty + every URL a
    // well-formed iroh RelayUrl. A malformed entry must abort with nothing half-applied.
    anyhow::ensure!(
        !relay_urls.is_empty(),
        "set_relays: relay_urls is empty (custom mode requires at least one relay; \
         disable relays via a relay_mode=\"disabled\" restart)"
    );
    let parsed: Vec<iroh::RelayUrl> = relay_urls
        .iter()
        .map(|u| {
            u.parse::<iroh::RelayUrl>()
                .map_err(|e| anyhow::anyhow!("set_relays: relay url {u:?}: {e}"))
        })
        .collect::<Result<_>>()?;

    let _reload = mesh.reload_lock.lock().await;

    // The LIVE relay posture (seeded at boot, updated on each edit) is the runtime truth we diff
    // against — NOT the on-disk config, which the `.config()` embedder front door may never have
    // written. Only `custom` mode can be live-reconfigured; any other current mode means the
    // switch onto custom is a MODE transition iroh can't do live → persist + `restart_required`.
    let posture = mesh.applied_relays();
    let restart_required = posture.mode != "custom";

    // Diff on the NORMALIZED relay URL (iroh's canonical `RelayUrl` form — trailing slash, lowercased
    // host, default port dropped), NOT the raw strings: the running endpoint's relay map keys on the
    // normalized value, so a re-spelling of the same relay (a trailing slash, host case) must count
    // as unchanged. Diffing raw strings would `remove_relay` a relay the caller meant to KEEP.
    let desired_norm: Vec<String> = parsed.iter().map(|r| r.to_string()).collect();
    let current_norm: Vec<String> = posture
        .urls
        .iter()
        .filter_map(|u| u.parse::<iroh::RelayUrl>().ok().map(|r| r.to_string()))
        .collect();
    let desired_set: BTreeSet<&str> = desired_norm.iter().map(String::as_str).collect();
    let current_set: BTreeSet<&str> = current_norm.iter().map(String::as_str).collect();

    // Idempotent on BOTH paths: an unchanged set → no writes, no endpoint calls. (On the
    // restart-required path `current_norm` is the last-persisted set we tracked, so a repeat call
    // with the same set before a restart is a clean no-op too.)
    if current_set == desired_set {
        return Ok(SetRelaysResult {
            changed: false,
            restart_required,
        });
    }

    // Persist FIRST — it is the ONLY fallible step (iroh's insert/remove can't fail on an open
    // endpoint). Persisting before the live mutation keeps the critical section atomic: a write
    // error leaves the endpoint, the posture, AND the config all untouched (nothing applied). We
    // persist the NORMALIZED forms so config/posture always match the live map's keys.
    let config_path = mesh.config_path.clone();
    let persisted = desired_norm.clone();
    blocking("set_relays config write", move || {
        write_relays(&config_path, &persisted)
    })
    .await??;

    // Apply the delta to the running endpoint ONLY on the custom→custom path (iroh can't live-
    // transition the relay MODE). These calls are infallible on an open endpoint.
    if !restart_required {
        // Insert the newly-added relays (desired − current).
        for (ru, norm) in parsed.iter().zip(desired_norm.iter()) {
            if !current_set.contains(norm.as_str()) {
                mesh.endpoint
                    .insert_relay(ru.clone(), Arc::new(iroh::RelayConfig::from(ru.clone())))
                    .await;
            }
        }
        // Remove the dropped relays (current − desired). `current_norm` came from parsing, so each
        // re-parses to the same `RelayUrl` the live map holds.
        for norm in &current_norm {
            if !desired_set.contains(norm.as_str())
                && let Ok(ru) = norm.parse::<iroh::RelayUrl>()
            {
                mesh.endpoint.remove_relay(&ru).await;
            }
        }
    }

    // Track the persisted set as the new posture. On the custom path the live endpoint now matches;
    // on the restart-required path the endpoint is UNCHANGED, so keep the (non-custom) mode — that
    // makes a repeat call idempotent yet still `restart_required` until an actual restart.
    let new_mode = if restart_required {
        &posture.mode
    } else {
        "custom"
    };
    mesh.set_applied_relays(new_mode, &desired_norm);

    tracing::info!(
        count = desired_norm.len(),
        restart_required,
        "set custom relay set"
    );
    Ok(SetRelaysResult {
        changed: true,
        restart_required,
    })
}

/// Grant a freshly-paired peer AUTHORIZATION to the services its invite named: append
/// `redeemer_nickname` to each service's config `[services.<svc>].allow` (idempotently) and
/// hot-reload so the running registry admits it. This is the load-bearing half of pairing.
///
/// Why it is separate from (and necessary alongside) the [`PeerEntry`] the rendezvous writes:
/// the [`AllowlistGate`](crate::allowlist::AllowlistGate) only RESOLVES an inbound endpoint to
/// a nickname (identity); `select_service` then ADMITS that nickname only if the
/// service's config `allow` names it — and that allow is baked into the [`Services`](mcpmesh_net::Services) snapshot
/// at [`build_services`](crate::daemon::build_services) time. So a PeerEntry makes the peer KNOWN; only appending to `allow`
/// + reloading makes it AUTHORIZED. Without this the peer is known-but-forbidden.
///
/// Serialized against `register_service` via `mesh.reload_lock` (SAME lock — a concurrent
/// register and a pairing-grant must not read the same base config and clobber each other's
/// write). Reuses `append_allow_to_config`'s atomic write and `swap_services`'s
/// in-place registry swap (DRY). A service not present in config is logged + skipped (a pairing grant
/// never CREATES a service). Reloads ONLY when the append actually changed the config — an
/// idempotent re-pair or an all-missing grant is a no-op with no serving blip. (The cached
/// `status` snapshot is not refreshed here — this runs inside the accept loop's detached pair
/// handler, which holds no `DaemonState` — but it need not be: `status` reads the config + store
/// LIVE (control.rs `status_result`), so this grant shows up immediately. The durable allow-append
/// + the live rebuilt `Services` are the functional truth.)
pub async fn grant_service_access(
    mesh: &Arc<MeshState>,
    principal: &str,
    display_nickname: &str,
    services: &[String],
) -> Result<()> {
    // SAME serialization as register_service: hold the whole append→reload→swap section.
    let _reload = mesh.reload_lock.lock().await;

    // 1. Idempotent allow-append on a blocking thread (config IO blocks). `principal` is the
    //    redeemer's STABLE identity (#38: `b64u:` when bound, else `eid:`) — the display
    //    nickname below is audit/log color only and never lands in `allow`.
    //    This path stays LENIENT about a name matching neither source (warn + skip, as before):
    //    it is the pairing ceremony, and a stale service name in an invite must never abort a
    //    pairing. The strict, single-service [`grant_service_allow`] is where an unknown name
    //    errors.
    //
    //    ORDER MATTERS: the CONFIG write runs FIRST, and the in-memory ephemeral allow is only
    //    mutated once it has succeeded (#55 review). The reverse order left a failed grant
    //    half-applied — the verb returned `Err`, but the in-memory grant stood and was installed
    //    by the next unrelated reload, admitting a principal the caller was told was not granted.
    let config_path = mesh.config_path.clone();
    let principal_w = principal.to_string();
    let config_services = services.to_vec();
    // Names the daemon already knows are ephemeral registrations are absent from config BY DESIGN;
    // logging that at `warn!` per grant is noise the caller cannot act on (#94).
    let known_ephemeral: HashSet<String> = {
        let map = mesh
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned");
        services
            .iter()
            .filter(|s| map.contains_key(*s))
            .cloned()
            .collect()
    };
    let changed = blocking("join grant config write", move || {
        append_allow_to_config(
            &config_path,
            &principal_w,
            &config_services,
            &known_ephemeral,
        )
    })
    .await??;

    //    EPHEMERAL registrations carry their allow in memory only (#55), so the config append
    //    above cannot reach them. Apply to BOTH sources rather than ephemeral-first: a name can be
    //    held by both (a hand-edited config under a live ephemeral registration), and granting only
    //    the shadowing copy would leave the config copy stale — then live, with the wrong allow,
    //    the moment the ephemeral entry is dropped.
    let mut changed = changed;
    for svc in services {
        if let Some(moved) = mesh.grant_ephemeral(svc, principal) {
            changed |= moved;
        }
    }

    // 2/3. Reload + hot-swap ONLY when the allow actually changed (else the running registry
    //      already admits the peer). The reload MUST happen for a real append to take effect,
    //      since `select_service` reads the allow baked into `Services` at build time.
    if changed {
        reload_services_from_disk(mesh, "grant").await?;
    }

    // Trust event: NO secret (the display nickname is the surface-clean handle).
    tracing::info!(peer = %display_nickname, ?services, changed, "granted service access");
    // Trust event: a pairing grant. Display nickname only — NO secret.
    mesh.audit().record(AuditRecord::trust(
        now_ts(),
        "pair".into(),
        Some(display_nickname.to_string()),
        // #57: the redeemer's stable principal — the same value the grant just appended to the
        // allow, so the trust history joins to the policy it created.
        Some(principal.to_string()),
    ));
    Ok(())
}

/// Does `config.toml` carry a `[services.<name>]` entry the daemon can actually SERVE? Read fresh
/// (not from the live registry) so a service added out-of-band since boot counts. Used only to
/// distinguish "nothing to change" from "no such service" (#55) — the surgical RMW writers report
/// `false` for both.
///
/// A config-load failure PROPAGATES rather than answering `false` (#55 review): a corrupt or
/// unreadable config is not the same condition as a missing service, and reporting
/// [`NoSuchService`] for it would tell the operator to register a service that already exists.
///
/// The entry must also have a well-formed backend. `build_services_with_ephemeral` skips a
/// malformed `[services.*]` (neither/both of `run` and `socket`) with a warning, so treating it as
/// present would let a grant report success and write an allow that admits nobody — the exact
/// silent-success class this strictness exists to remove.
/// Push an EPHEMERAL service's current in-memory `allow` into the live registry without touching
/// disk (#94).
///
/// The disk-reload path re-parses `config.toml` and reconstructs every service's backend. When the
/// allow edit landed only in the ephemeral overlay, the config file did not change, so that rebuild
/// reproduces the same config half at a cost that scales with the total number of services — the
/// per-grant tax #94 reported for a room whose membership is a list of per-principal grants.
///
/// Falls back to a full reload if the name is not in the live registry: `with_allow_replaced`
/// refuses to invent an entry, and a rebuild is the only thing that can legitimately create one.
async fn apply_ephemeral_allow(mesh: &Arc<MeshState>, service: &str, why: &str) -> Result<()> {
    let allow = {
        let map = mesh
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned");
        map.get(service).map(|e| e.allow.clone())
    };
    let updated = allow.and_then(|allow| mesh.services.get().with_allow_replaced(service, allow));
    match updated {
        Some(services) => {
            swap_services(mesh, services);
            Ok(())
        }
        None => reload_services_from_disk(mesh, why).await,
    }
}

async fn service_servable_in_config(mesh: &Arc<MeshState>, service: &str) -> Result<bool> {
    let config_path = mesh.config_path.clone();
    let service = service.to_string();
    blocking("join service-exists config read", move || {
        let cfg = Config::load(&config_path)
            .map_err(|e| anyhow::anyhow!("config error in {}: {e}", config_path.display()))?;
        Ok(cfg
            .services
            .get(&service)
            .is_some_and(|svc| svc.backend_result().is_ok()))
    })
    .await?
}

/// The named service exists in neither the config nor the ephemeral registry (#55). A distinct
/// error type so `respond` can map it to [`ERR_NO_SUCH_SERVICE`](mcpmesh_local_api::ERR_NO_SUCH_SERVICE)
/// and a caller can branch — the same `downcast_ref` idiom `InvalidParams` uses for `-32602`.
#[derive(Debug)]
pub struct NoSuchService(pub String);

impl std::fmt::Display for NoSuchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no service named '{}' that this daemon can serve — register it first \
             ('mcpmesh serve', or register_service), or check the config entry names exactly one \
             of `run` / `socket`",
            self.0
        )
    }
}
impl std::error::Error for NoSuchService {}

/// Grant a SINGLE stable `principal` access to a SINGLE `service` (#44, the
/// `service_allow_grant` verb) — the per-peer "sharing on" toggle primitive. Idempotent +
/// serialized under `reload_lock`, exactly like a pairing grant.
///
/// Unlike the pairing [`grant_service_access`] it wraps, this is STRICT (#55): a name that is
/// neither an ephemeral registration nor a `[services.*]` config entry is a [`NoSuchService`]
/// error, not a silent success. It used to answer `{}` for every unknown name — including EVERY
/// ephemeral service, whose allow is in memory and so was never touched by the config append.
pub(crate) async fn service_allow_grant(
    state: &DaemonState,
    service: String,
    principal: String,
) -> Result<()> {
    grant_service_allow(state.mesh_required()?, service, principal).await
}

/// The mesh-level half of `service_allow_grant`, and the exact mirror of [`revoke_service_allow`]:
/// resolve the service, append `principal` to its allow, hot-swap the live registry.
///
/// Resolution and mutation BOTH happen under `reload_lock` (#55 review). Resolving outside the
/// lock left a race: an ephemeral registration whose control connection dropped between the check
/// and the mutation fell through to the config append, which warn-and-skipped, and the verb
/// reported success having granted nobody — the #55 symptom restored as a race.
///
/// STRICT, unlike the pairing [`grant_service_access`]: a name that is neither an ephemeral
/// registration nor a servable `[services.*]` entry is a [`NoSuchService`] error rather than a
/// silent success. It used to answer `{}` for every unknown name — silently including EVERY
/// ephemeral service, whose allow the config append never touched.
///
/// `pub` (like [`revoke_service_allow`]) so integration tests drive the SAME pipeline the verb does.
pub async fn grant_service_allow(
    mesh: &Arc<MeshState>,
    service: String,
    principal: String,
) -> Result<()> {
    let _reload = mesh.reload_lock.lock().await;

    // REVOCATION FIRST, before the service is even resolved and before any write (#212). Admission
    // consults the revocation table before any service authz runs, so a grant to a revoked
    // principal is an `allow` entry that admits nobody — and this verb used to mint exactly that,
    // answer `{}`, and let `status` confirm it. Same table, same question, same answer as the gate
    // (`principal_is_revoked` spells out what "revoked" means for each principal shape). A store
    // read, so it runs on the blocking pool like every other store access on this path.
    {
        let store = mesh.store.clone();
        let roster = mesh.roster.view();
        let principal_r = principal.clone();
        let revoked = blocking("join service-allow grant revocation check", move || {
            principal_is_revoked(&store, roster.as_deref(), &principal_r)
        })
        .await?;
        if revoked {
            anyhow::bail!(PrincipalRevoked(principal));
        }
    }

    let is_ephemeral = {
        let map = mesh
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned");
        map.contains_key(&service)
    };
    if !is_ephemeral && !service_servable_in_config(mesh, &service).await? {
        anyhow::bail!(NoSuchService(service));
    }

    // CONFIG FIRST, then the in-memory allow — a failed config write must not leave an ephemeral
    // grant half-applied (see `grant_service_access`).
    //
    // The config pass runs even when the name IS ephemeral, and #94 asked for it to be skipped.
    // It is deliberately kept: ephemeral and in-config are NOT mutually exclusive (the #55 review
    // case, spelled out in `revoke_service_allow` below), and a grant that skipped config would
    // silently expire the moment the registering control connection dropped the overlay. The cost
    // #94 measured is the reload, not this — `append_allow_to_config` returns without writing when
    // the name is absent, and the reload below is now skipped for the overlay-only case.
    let config_path = mesh.config_path.clone();
    let (principal_w, services_w) = (principal.clone(), vec![service.clone()]);
    let known_ephemeral: HashSet<String> = if is_ephemeral {
        std::iter::once(service.clone()).collect()
    } else {
        HashSet::new()
    };
    let config_moved = blocking("join service-allow grant config write", move || {
        append_allow_to_config(&config_path, &principal_w, &services_w, &known_ephemeral)
    })
    .await??;
    let ephemeral_moved = mesh.grant_ephemeral(&service, &principal).unwrap_or(false);

    // Branch on what actually changed ON DISK, not on `is_ephemeral` — a name held by both sources
    // can move the config copy, and that needs the real rebuild.
    if config_moved {
        reload_services_from_disk(mesh, "service-allow-grant").await?;
    } else if ephemeral_moved {
        apply_ephemeral_allow(mesh, &service, "service-allow-grant").await?;
    }
    let changed = config_moved || ephemeral_moved;
    tracing::info!(%service, %principal, changed, "service allow granted");
    Ok(())
}

/// Revoke a SINGLE allow entry from a SINGLE `service` (#44, the `service_allow_revoke` verb) —
/// the per-peer "sharing off" toggle, WITHOUT unpairing (the peer's `PeerEntry` identity is
/// untouched). A thin `DaemonState` wrapper over [`revoke_service_allow`], mirroring how
/// [`service_allow_grant`] wraps [`grant_service_access`].
///
/// **`principal` is matched as an EXACT STRING, not resolved (#149).** The parameter name says
/// "principal" because that is what an allow entry normally is, but nothing validates it: any
/// literal already in the list is a valid target, including a BARE entry (a legacy nickname from a
/// pre-#38 config, a roster group name). That is the documented remedy for an entry no other path
/// will strip — [`revoke_service_access`] deliberately refuses to guess at bare strings, and
/// `write_service_to_config` unions rather than replaces.
///
/// The corollary is worth stating too: an exact match is exactly as literal as it sounds. Revoking
/// `b64u:<user>` removes that entry outright, without the multi-device protection
/// [`revoke_service_access`] applies (it keeps a shared `b64u:` while another stored peer carries
/// it). Here the caller named the string, so the string goes.
///
/// **The SEVER that follows the strip is NOT literal, and that is the sharp edge.** Revocation is
/// immediate post-#54: [`revoke_service_allow`] cuts the principal's live connections, and that
/// lookup DOES resolve — through roster `user_id`s and group membership. So revoking a bare literal
/// that also names a live roster group strips one allow line but severs every device in that group,
/// including their sessions to OTHER services. Their access elsewhere is untouched and clients
/// reconnect, so this is bluntness rather than an authorization defect — but the "no guessing"
/// property belongs to the strip alone.
pub(crate) async fn service_allow_revoke(
    state: &DaemonState,
    service: String,
    principal: String,
) -> Result<()> {
    revoke_service_allow(state.mesh_required()?, service, principal).await
}

/// The mesh-level half of `service_allow_revoke`: strip `principal` from `service`'s allow,
/// hot-swap the live registry, then SEVER the principal's live connections. Idempotent +
/// serialized under `reload_lock`, mirroring [`grant_service_access`].
///
/// **Resolves the service EPHEMERAL-first, then config, then errors** (#69). An ephemeral
/// registration's allow lives in memory only, so before this the strip edited `config.toml`, found
/// nothing, and the next hot-reload re-overlaid the untouched in-memory allow — the peer stayed
/// admitted while the verb reported success. A name that is neither is now a
/// [`NoSuchService`] error rather than a silent no-op.
///
/// Post-#54: revocation is IMMEDIATE. New sessions are refused (the live registry, read per
/// bi-stream) and in-flight ones are cut (the sever). Previously both waited for the peer to
/// disconnect on its own.
///
/// `pub` (like [`grant_service_access`]) so the integration tests drive the SAME
/// strip→swap→sever pipeline the control verb drives.
pub async fn revoke_service_allow(
    mesh: &Arc<MeshState>,
    service: String,
    principal: String,
) -> Result<()> {
    let _reload = mesh.reload_lock.lock().await;

    // Strip from BOTH sources, not ephemeral-first (#55 review). A name can be held by both — a
    // hand-edited `config.toml` under a live ephemeral registration — and stripping only the
    // shadowing ephemeral copy left the config copy holding the principal. That copy is invisible
    // while the overlay shadows it, then goes LIVE with the stale allow the moment the registering
    // control connection drops the ephemeral entry, re-admitting a principal the operator was told
    // was revoked. Revocation must be fail-closed across every allow the name owns.
    let ephemeral_moved = mesh.revoke_ephemeral(&service, &principal);
    let config_path = mesh.config_path.clone();
    let (svc_w, principal_w) = (service.clone(), principal.clone());
    let config_moved = blocking("join service-allow revoke config write", move || {
        remove_principal_from_service(&config_path, &svc_w, &principal_w)
    })
    .await??;

    // `remove_principal_from_service` reports `false` both for "service absent" and for "principal
    // was not in this service's allow", so re-read the config to tell them apart: only the former
    // is an error, and only when no ephemeral registration claims the name either.
    if ephemeral_moved.is_none()
        && !config_moved
        && !service_servable_in_config(mesh, &service).await?
    {
        anyhow::bail!(NoSuchService(service));
    }
    let changed = config_moved || ephemeral_moved.unwrap_or(false);
    // SWAP-BEFORE-SEVER (#54): swap first so no NEW session admits the principal, THEN cut the
    // sessions already in flight.
    //
    // Gated on `changed` DELIBERATELY. A strip that removed nothing means this principal was not
    // in that allow, so nothing was revoked — and severing anyway would hand the operator a
    // visible disconnect that LOOKS like the revoke landed while access is unchanged. Concretely:
    // `allow = ["b64u:alice"]` and a caller revoking `eid:<alice's device>` strips nothing, but
    // that device is still admitted via the user_id and would be served again the instant it
    // redialed. A false "revocation took effect" signal is worse on this surface than a missed
    // sever, and `api_minor >= 10` is what consumers key that signal off.
    //
    // SWAP-BEFORE-SEVER is preserved in BOTH branches below: the targeted overlay swap (#94) goes
    // through the same `LiveServices::store` the rebuild does, so no new session admits the
    // principal before the in-flight ones are cut.
    //
    // NOTE: this ORDER is not enforced by any test — reversing it passes the whole suite, since
    // both have happened by the time the verb returns. Keep the order when editing here.
    let severed = if changed {
        if config_moved {
            reload_services_from_disk(mesh, "service-allow-revoke").await?;
        } else {
            apply_ephemeral_allow(mesh, &service, "service-allow-revoke").await?;
        }
        sever_principal(mesh, &principal).await?
    } else {
        0
    };
    tracing::info!(%service, %principal, changed, severed, "service allow revoked");
    Ok(())
}

/// Close every live mesh connection held by one `principal`'s devices. Thin wrapper over
/// [`sever_principals`] for the single-principal call sites.
async fn sever_principal(mesh: &Arc<MeshState>, principal: &str) -> Result<usize> {
    sever_principals(mesh, std::slice::from_ref(&principal.to_string())).await
}

/// Close every live connection held by ANY of `principals`' devices, or carrying a live session
/// ADMITTED as one of them (#222 review), returning the number severed.
///
/// The liveness half of a revoke (#54): stripping the config `allow` and swapping the live
/// registry stop NEW sessions, but an in-flight session on an already-open connection keeps running
/// until the peer disconnects — unbounded for an embedder holding a warm session.
///
/// Resolves the WHOLE set in ONE pass (one `store.list()` + one roster-view walk) rather than once
/// per principal, since `revoke_service_access` routinely passes a device `eid:` and its owner's
/// `b64u:` together.
///
/// **Granularity is the CONNECTION, not the session.** `sever_matching` closes the whole QUIC
/// connection, so a peer revoked from ONE service also loses in-flight sessions to services it
/// still holds; it redials and is re-evaluated against the live registry. Per-session cancellation
/// would need the registry to track sessions by service, and would still not protect the revoked
/// service's own in-flight stream — the actual hazard. Revocation is an explicit operator action,
/// so the bluntness is the accepted cost of the verb taking effect NOW.
///
/// **It also reaches non-mesh ALPNs.** The registry tracks gossip and blob connections on the same
/// endpoint id with no ALPN discriminator, so a revoke cuts those too. Availability only (each of
/// those arms keeps its own gate), and the peer reconnects; documented in `docs/local-protocol.md`
/// so it is not a surprise.
///
/// A principal naming no device and admitting no live session severs nothing.
async fn sever_principals(mesh: &Arc<MeshState>, principals: &[String]) -> Result<usize> {
    // #99: hand the observer the registry AS OF NOW — before any connection is cut. A caller that
    // severed before swapping would show a registry here that still admits the principal.
    let observer = mesh
        .sever_observer
        .lock()
        .expect("sever observer lock not poisoned")
        .clone();
    if let Some(observe) = observer {
        observe(&mesh.services.get());
    }
    // #229: the inbound registry below holds only ACCEPTED connections. Connections this node
    // DIALLED — an `open_session`, an embedder's `connect_protocol`, a gossip link — are cut here,
    // for every device the caller's revocation write now refuses. First, so a failing principal
    // resolution below cannot skip it. Not counted in `severed`: that field has always counted the
    // inbound registry, and a connection can sit in both.
    let closed = mesh.close_refused_peer_conns().await;
    if closed > 0 {
        tracing::info!(closed, "closed live connections to now-refused devices");
    }
    let store = mesh.store.clone();
    let roster = mesh.roster.view();
    let principals_w = principals.to_vec();
    let targets = blocking("join sever principal resolution", move || {
        let mut all = std::collections::HashSet::new();
        for principal in &principals_w {
            all.extend(crate::daemon::sever::endpoints_for_principal(
                &store,
                roster.as_deref(),
                principal,
            )?);
        }
        anyhow::Ok(all)
    })
    .await??;
    // Match on BOTH what the principal maps to NOW (the store/roster endpoints) AND what live
    // sessions were ADMITTED as (#222 review). Sessions re-resolve their principal per session, so a
    // device the store now calls `b64u:NEW` can still carry a session admitted as `b64u:OLD`; the
    // endpoint lookup alone would sever nothing for a revoke of `b64u:OLD` while the verb reported
    // success. No early return on empty `targets` for the same reason.
    Ok(mesh.conn_registry.sever_matching_admitted(
        mcpmesh_net::CLOSE_UNAUTHORIZED, // 401 — "no longer authorized"
        b"access revoked",
        |eid| targets.contains(eid),
        |admitted_as| principals.iter().any(|p| p == admitted_as),
    ))
}

/// Revoke a peer's AUTHORIZATION: resolve the nickname to its devices' STABLE principals
/// (#38) and strip them from EVERY service's config `[services.<svc>].allow`, then hot-reload
/// so the running registry stops admitting them. The exact INVERSE of
/// [`grant_service_access`] (which appends the stable principal), and the authorization half
/// of [`remove_peer`].
///
/// **The principal-strip rule (spec-settled):** each target device's `eid:` is stripped
/// ALWAYS; the shared `b64u:` user_id is stripped ONLY when no OTHER stored peer entry
/// carries it — unpairing one device of a multi-device person must never revoke the person.
/// Bare strings in `allow` are NEVER stripped here: post-#38 a bare entry is roster
/// vocabulary (a group or roster user_id), and a nickname-keyed strip could collide with a
/// group name and revoke a whole roster group. (Note the boundary: admission requires gate
/// RESOLVE first, so deleting the PeerEntry already denies the device outright — this strip
/// is grant hygiene, not the security boundary.)
///
/// **A bare entry is still removable — just not from HERE (#149).** This paragraph was read as
/// "bare entries are permanent", which is a fair reading of it and wrong about the system:
/// [`revoke_service_allow`] strips by EXACT STRING, so
/// `service_allow_revoke {service, principal: "<the literal>"}` removes any entry, bare included.
/// The collision hazard above does not apply to that STRIP, which is the point — the caller names
/// a literal instead of a name to resolve, so exactly one line goes and it is the one named. (It
/// does still apply to the sever that follows; see [`service_allow_revoke`].) What is unavailable
/// is doing it as a SIDE EFFECT of unpairing, and that is deliberate.
///
/// Serialized against [`register_service`] / [`grant_service_access`] via `mesh.reload_lock` (the
/// SAME lock — a concurrent config mutation must not read the same base config and clobber this
/// removal). Reuses [`remove_allow_from_config`]'s atomic write and [`swap_services`]'s
/// in-place registry swap (DRY — the same helper the grant uses). Reloads ONLY when the removal actually
/// changed the config (an absent nickname is a no-op with no serving blip). Idempotent: revoking a
/// nickname not present in any allow returns `Ok(())` with `changed == false` and no reload.
///
/// (Like [`grant_service_access`], the cached `status` snapshot is not refreshed here — but
/// `status` reads the config + store LIVE (control.rs `status_result`), so the removal shows up
/// immediately. The durable allow-removal + the live rebuilt `Services` are the functional truth.)
pub async fn revoke_service_access(mesh: &Arc<MeshState>, nickname: &str) -> Result<bool> {
    // SAME serialization as register_service / grant: hold the whole remove→reload→swap section.
    let _reload = mesh.reload_lock.lock().await;

    // 0. Resolve the target devices' stable principals BEFORE any teardown (the caller
    //    `remove_peer` deletes the rows after this returns — ordering already safe).
    let store = mesh.store.clone();
    let nick_r = nickname.to_string();
    let principals: Vec<String> = blocking("join revoke principal resolution", move || {
        let (targets, others): (Vec<_>, Vec<_>) = store
            .list()?
            .into_iter()
            .partition(|e| e.nickname == nick_r);
        let mut principals = Vec::new();
        for target in &targets {
            principals.push(mcpmesh_net::EndpointId::from_bytes(target.endpoint_id).principal());
            if let Some(user_id) = &target.user_id {
                // "Shared elsewhere" means ANOTHER device of the same person is still trusted, so
                // stripping the shared user_id would revoke that device too. But an INTRODUCED row
                // (`paired_at: None`) is not a device you vouched for — it is one someone else's
                // endorsement installed. Counting it as "still trusted" let an endorsement outlive
                // the unpairing that was supposed to end the relationship: endorse a second
                // endpoint under your own user_id, get it installed, and `pair --remove` then finds
                // the user_id "shared", skips the strip, and leaves the grant intact forever
                // (#65 gate, HIGH).
                //
                // Only a PAIRED sibling — a device the operator ran a ceremony for — counts.
                let shared_with_a_paired_device = others
                    .iter()
                    .any(|o| o.user_id.as_deref() == Some(user_id) && o.paired_at.is_some());
                if !shared_with_a_paired_device && !principals.contains(user_id) {
                    principals.push(user_id.clone());
                }
            }
        }
        anyhow::Ok(principals)
    })
    .await??;
    if principals.is_empty() {
        // No stored device under this nickname → nothing resolvable to strip. (Legacy
        // bare-nickname allow entries are deliberately untouched — doctor lints them.)
        tracing::info!(peer = %nickname, changed = false, "revoked service access");
        return Ok(false);
    }

    // 1. Idempotent allow-removal on a blocking thread (config IO blocks) — ONE atomic RMW
    //    over all of the peer's principals.
    let config_path = mesh.config_path.clone();
    let principals_w = principals.clone(); // the sever below needs them after the write consumes its copy
    let changed = blocking("join revoke config write", move || {
        remove_allow_from_config(&config_path, &principals_w)
    })
    .await??;

    // 2/3. Reload + hot-swap ONLY when the allow actually changed (else the running registry
    //      already excludes the peer). A real removal MUST reload for `select_service` — which
    //      reads the allow baked into `Services` at build time — to stop admitting the nickname.
    if changed {
        reload_services_from_disk(mesh, "revoke").await?;
    }

    // 4. SEVER the peer's live connections (#54). The strip + swap above stop NEW sessions;
    //    without this, sessions already in flight run to completion on a connection whose peer we
    //    just de-authorized. Runs AFTER the swap (swap-before-sever, the ordering
    //    `install_roster_view_and_sever` uses) so a peer racing a redial across the sever meets
    //    the NEW registry.
    //
    //    UNCONDITIONAL of `changed` here — unlike `revoke_service_allow`, which gates on it. The
    //    caller (`remove_peer`) DELETES the `PeerEntry` right after this returns, so the peer
    //    loses gate resolve entirely and cannot be re-admitted on redial. There is therefore no
    //    false "it took effect" signal to worry about: the unpair really did take effect, whether
    //    or not any allow line happened to name it.
    let severed = sever_principals(mesh, &principals).await?;

    // Return whether an allow was actually stripped so `remove_peer` audits an `unpair` only
    // on a real tear-down (nickname only — NO secret, NO endpoint id).
    tracing::info!(peer = %nickname, changed, severed, "revoked service access");
    Ok(changed)
}

/// Handle an `open_session` control request: resolve the nickname, dial the named
/// service over the mesh, and pipe that session to/from the control connection — which, after
/// this request, STOPS being JSON-RPC and becomes a raw MCP byte pipe (protocol.rs
/// `OpenSession`). On any dial-ESTABLISHMENT failure (peer not allowlisted, malformed stored
/// id, unreachable) the caller is handed a synthesized `-32055` (ERR_UNREACHABLE) frame, so
/// the AI client gets a well-formed answer instead of a hang; the remote's own `-32054`
/// refusal, and every session frame, flow back verbatim through the pipe. There is no
/// mid-session re-dial — the remote session state died with the session, so a severed session
/// simply ends the pipe (the AI client re-invokes if it wants a fresh one).
pub(crate) async fn open_session<CR, CW>(
    state: &DaemonState,
    peer: &str,
    service: &str,
    // #166: a per-session QUIC idle timeout, or `None` for the node-wide value.
    idle_timeout_secs: Option<u64>,
    control_reader: FrameReader<CR>,
    mut control_writer: CW,
) -> Result<()>
where
    CR: AsyncRead + Unpin + Send,
    CW: AsyncWrite + Unpin + Send,
{
    let Some(mesh) = state.mesh() else {
        // Control-only construction (no endpoint) can never dial — answer unreachable.
        let _ = write_frame(
            &mut control_writer,
            &synthesized(Value::Null, ERR_UNREACHABLE, "daemon has no mesh"),
        )
        .await;
        return Ok(());
    };
    let transport = match crate::daemon::dial::dial_service_with_idle_timeout(
        mesh,
        peer,
        service,
        idle_timeout_secs,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            // A failed dial reaches no backend, so the far side's session guard never audits
            // it (no session_open/close). Emit an error record HERE — exactly once, ONLY on
            // this failure branch (the Ok arm pipes the session instead) — so the telemetry
            // stream shows the attempted-and-failed reach. `peer` is the caller's
            // nickname/user_id, never an endpoint-id.
            mesh.audit().record(
                // #57: deliberately NO principal — this is OUR outbound dial that failed, not a
                // gate-resolved caller; there is no authenticated subject to attribute.
                AuditRecord::session_open(
                    now_ts(),
                    Some(peer.to_string()),
                    service.to_string(),
                    None,
                )
                .with_status("error"),
            );
            // Dial establishment failed: hand the proxy a well-formed -32055 (not a hang),
            // which it relays to the AI client. The error id is null — the AI
            // client's request id is not known daemon-side (the dial precedes the client's
            // first frame); this matches the null-id synthesis discipline in net::endpoint.
            tracing::warn!(peer, service, %e, "open_session dial failed; answering -32055");
            let _ = write_frame(
                &mut control_writer,
                &synthesized(Value::Null, ERR_UNREACHABLE, "peer unreachable"),
            )
            .await;
            return Ok(());
        }
    };
    pipe_session(transport, service, control_reader, control_writer).await
}

/// Handle a `user_key_export` control request (#85 ask 2): render this node's user key as a
/// recovery phrase.
///
/// **The phrase IS the private key.** Deliberately not audited and not logged — the audit log is a
/// durable, operator-readable file, and putting a private key in it would make the compliance
/// surface the largest copy of the secret. `status` does not carry it either; it exists only in
/// this response.
///
/// Errors when this node has no user key: there is nothing to export, and minting one here would
/// hand back a phrase for an identity nobody has ever seen.
///
/// Refuses (`-32602`) on an ENROLLED device (#86, #219): the local key is not the identity this
/// node presents, so its phrase would restore a stranger.
pub(crate) async fn user_key_export(
    state: &DaemonState,
) -> Result<mcpmesh_local_api::UserKeyExportResult> {
    let mesh = state.mesh_required()?;
    // #86 gate (#219): an ENROLLED device presents an identity it holds no key for. The LOCAL key
    // boot minted underneath backs an identity no peer has ever paired with, so exporting it hands
    // a person "backing up their identity" the wrong phrase — discovered only on the new hardware,
    // as a stranger to every peer. Same predicate and shape as `peer_endorse`, checked BEFORE the
    // key is read — and (#221) under `user_key_lock`, held until the phrase is built, so an
    // adoption cannot land between the check and the read.
    let _key_guard = lock_own_user_key(mesh, "user_key_export", "Export").await?;
    let path = mesh
        .user_key_path
        .get()
        .cloned()
        .context("this node has no user key path resolved")?;
    anyhow::ensure!(
        path.exists(),
        "this node has no user key to export ({})",
        path.display()
    );
    // READ, never `load_or_generate` — that mints when the file is absent, so a "read-only" export
    // racing a missing key would leave a fresh random identity behind that became this node's at
    // the next restart. Refusing AFTER the side effect is not refusing.
    mesh.run_identity_gate_hook().await;
    let bytes = blocking("join user key export", {
        let path = path.clone();
        move || std::fs::read(&path)
    })
    .await?
    .with_context(|| format!("read user key at {}", path.display()))?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("the user key at {} is not 32 bytes", path.display()))?;
    let key = mcpmesh_trust::UserKey::from_signing_key(
        mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&bytes),
    );
    let user_id = mcpmesh_trust::binding::user_id(&key);
    // AUDITED — the event, never the phrase. Exporting lifts the private key off this machine, and
    // leaving the more dangerous of the two verbs untraced while the import is recorded confuses
    // "do not log the secret" (right) with "do not log the act" (wrong).
    mesh.audit().record(crate::audit::AuditRecord::trust(
        crate::audit::now_ts(),
        "user_key_export".into(),
        Some(user_id.clone()),
        None,
    ));
    Ok(mcpmesh_local_api::UserKeyExportResult {
        recovery_phrase: crate::pairing::recovery::encode(&bytes),
        user_id,
    })
}

/// Handle a `user_key_import` control request (#85 ask 2): restore a user key from a recovery
/// phrase, so a person's `b64u:` survives the hardware.
///
/// **Refuses to overwrite an existing key unless `replace` is set.** Importing over a live key
/// discards the identity this node presents, irreversibly without that key's own phrase — and the
/// person doing it is, by construction, someone who has just been handling recovery phrases.
///
/// The new binding is installed LIVE, through the same path #86 uses for an adopted enrollment
/// binding, so the identity this node presents at pairing changes immediately rather than at the
/// next restart. A restart-required answer would leave the node presenting the OLD identity while
/// its operator believed the recovery had taken.
///
/// What it does NOT do: get this device admitted by anyone. Peers authorize per DEVICE, and a
/// restored user key does not put this endpoint in anybody's allowlist. That is #85 ask 3.
pub(crate) async fn user_key_import(
    state: &DaemonState,
    recovery_phrase: String,
    replace: bool,
) -> Result<mcpmesh_local_api::UserKeyImportResult> {
    let mesh = state.mesh_required()?;
    // Decoded FIRST, before anything is touched: a mistyped phrase must not have moved a key file.
    let bytes = crate::pairing::recovery::decode(&recovery_phrase)?;
    let _guard = mesh.user_key_lock.lock().await;
    let path = mesh
        .user_key_path
        .get()
        .cloned()
        .context("this node has no user key path resolved")?;
    let existed = path.exists();
    // A key BOOT minted is not an identity anyone has seen — and on the primary use case, a new
    // laptop, that is the only key there is: `identity import` auto-starts a daemon, which mints
    // one before the import runs. Guarding it made the common path always refuse and pushed people
    // to a flag whose help says it destroys things irreversibly. Training that habit is worse than
    // not having the guard.
    //
    // Unset (a control-only or test daemon that never resolved a key) is treated as "not minted
    // here", which fails CLOSED: the guard applies, and the caller can still pass `replace`.
    //
    // "Still" (#221): an earlier import in this lifetime cleared it, so the key it wrote is
    // defended like a loaded one. Read under `user_key_lock`, which every writer of the key holds.
    let minted_here = mesh.user_key_still_boot_minted();
    let protects = existed && !minted_here;
    anyhow::ensure!(
        !protects || replace,
        "this node already has a user key ({}) that it did not mint just now; importing over it \
         discards the identity it currently presents, which cannot be undone without that key's \
         own recovery phrase. Pass replace to do it anyway",
        path.display()
    );

    // #214: the adopted-binding SIDECAR goes FIRST, and a failure to remove it FAILS the import.
    // An import supersedes an enrollment, but boot re-reads that file and it outranks the key on
    // disk — so an import that swapped the key while the file survived presented X now and the
    // enrolled Y after the next restart, with `self_user_key_held: true` and nothing in `status`
    // to show it. The first version removed it last and only warned.
    //
    // The ordering keeps `user.key`, the sidecar and the live slot agreeing with what boot would
    // present on every branch:
    // - read or remove fails → nothing has been touched; the error names the file.
    // - removed, then the key write fails → the sidecar is written BACK from the bytes read, so the
    //   node is unchanged. If that restore also fails, the live adopted slot is cleared to match
    //   the disk (no sidecar, the old key), and the error says the enrollment was dropped.
    // - removed, key written → the imported binding is installed live, which clears the slot.
    let sidecar = mesh.adopted_binding_path();
    let removed_sidecar: Option<Vec<u8>> = blocking("join import sidecar remove", {
        let sidecar = sidecar.clone();
        move || {
            let bytes = match std::fs::read(&sidecar) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => anyhow::bail!(
                    "read the enrollment binding at {} before superseding it: {e}. Nothing was \
                     imported — the next boot would re-adopt that file over the imported key",
                    sidecar.display()
                ),
            };
            std::fs::remove_file(&sidecar).map_err(|e| {
                anyhow::anyhow!(
                    "remove the enrollment binding at {}: {e}. Nothing was imported — the next \
                     boot would re-adopt that file over the imported key",
                    sidecar.display()
                )
            })?;
            Ok(Some(bytes))
        }
    })
    .await??;

    let key = mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&bytes);
    let write_key = key.clone();
    let write_path = path.clone();
    let overwrite = existed; // any file present must be renamed over, throwaway or not
    let written = blocking("join user key import", move || {
        // ONE atomic step. The first version removed the old key and then wrote — leaving a window
        // where any write failure destroyed the identity outright, and the next boot came up as a
        // fresh random stranger rather than keyless. `write_signing_key(replace)` renames over the
        // target instead: the old key is readable until the new one replaces it.
        mcpmesh_trust::keys::write_signing_key(&write_path, &write_key, overwrite)
            .map_err(|e| anyhow::anyhow!("write user key at {}: {e}", write_path.display()))
    })
    .await
    .and_then(|r| r);
    if let Err(e) = written {
        if let Some(bytes) = removed_sidecar {
            let restore_path = sidecar.clone();
            let restored = blocking("join import sidecar restore", move || {
                crate::pairing::persist::write_private(&restore_path, &bytes)
            })
            .await
            .and_then(|r| r.map_err(anyhow::Error::from));
            if let Err(re) = restored {
                mesh.set_self_binding_live(None);
                return Err(e.context(format!(
                    "the import failed after removing the enrollment binding at {}, and that \
                     file could not be restored ({re}): this device no longer presents the \
                     enrollment and now presents its own identity",
                    sidecar.display()
                )));
            }
        }
        return Err(e);
    }
    // #221: the key on disk is now a real identity. Cleared only once the write has landed — a
    // failed write left the boot-minted key in place, still worth nothing.
    mesh.note_user_key_replaced();

    // LIVE: the identity this node presents changes now, not at the next restart. A
    // restart-required answer would leave it presenting the OLD identity while its operator
    // believed the recovery had taken — they would go on to pair, and the peer would store the
    // wrong person.
    //
    // Its OWN slot, not #86's adopted one. That slot means "this device holds no user key", and
    // after an import it does; conflating them made a freshly-recovered machine refuse to endorse
    // or to enroll its owner's other devices — the very remedy the recovery CLI prints.
    // `set_imported_binding` also CLEARS the adopted slot, because an import supersedes an
    // enrollment: this device now holds the key.
    let user_key = mcpmesh_trust::UserKey::from_signing_key(key);
    let (user_pk, sig) = mcpmesh_trust::binding::present(&user_key, mesh.endpoint.id().as_bytes());
    mesh.set_imported_binding(crate::pairing::rendezvous::SelfBinding { user_pk, sig });
    let user_id = mcpmesh_trust::binding::user_id(&user_key);
    // #85/#66: in ROSTER mode the org's signed roster pins the device→user binding for THIS
    // endpoint, against the user_pk that was current when the operator approved it. Importing a
    // different key desyncs this device from its roster entry, and nothing re-signs — an operator
    // has to re-approve. Said out loud rather than left to be discovered as "roster mode stopped
    // working"; not refused, because a person recovering an identity may legitimately need to.
    if mesh.roster.view().is_some() {
        tracing::warn!(
            %user_id,
            "user key imported on a ROSTER-mode node — the signed roster still pins the previous \
             user key for this device, so an operator must re-approve it"
        );
    }
    // Trust event: an identity change is exactly the kind of thing an operator reads a log to find.
    // The user_id only — never the phrase, never the key.
    mesh.audit().record(crate::audit::AuditRecord::trust(
        crate::audit::now_ts(),
        "user_key_import".into(),
        Some(user_id.clone()),
        None,
    ));
    tracing::info!(%user_id, replaced = existed, "user key imported from a recovery phrase");
    Ok(mcpmesh_local_api::UserKeyImportResult {
        user_id,
        // `protects`, not `existed`: it answers "was a real identity discarded", which is what a UI
        // would warn about. A key this boot minted seconds ago existed on disk and was worth
        // nothing, so reporting that as a replacement would have every new-laptop recovery claim
        // it destroyed something.
        replaced: protects,
    })
}

/// `self_enroll_detach` (#214): drop an ADOPTED enrollment binding — the inverse of [`adopt_hook`].
///
/// The exit self-enrollment did not have. `adopt_hook` writes the binding to disk and installs it
/// live, and the only verb that cleared the slot was `user_key_import` — which needs the recovery
/// phrase of a key an enrolled device by construction does not hold. So a person handed a
/// substituted `mcpmesh-enroll:` line was a device of a stranger's identity until they deleted the
/// state root by hand.
///
/// Sidecar FIRST, then the live slot. The file is what boot re-reads, so a detach that cleared the
/// slot and then failed to remove the file would report an exit the next restart silently undid.
/// `user_key_import` tolerates that as best-effort because its live state is already the right
/// identity; here the exit IS the verb, so a removal failure leaves the slot set and is the error.
///
/// Idempotent ON DISK, not only live. A sidecar with no live adoption is reachable — boot declines
/// to present a file that does not verify for this endpoint, and before #214 an import removed it
/// only best-effort — and the next boot re-reads it. So the "nothing adopted"
/// refusal still removes a stale file first, and says so, rather than answering "already done"
/// over a file that would undo the answer at the next restart. If that removal fails for any
/// reason other than the file being absent, the answer is the removal error, UNCODED (`-32000`):
/// "not enrolled" would be false about what the next boot presents.
///
/// Local only: it changes what this device PRESENTS, not what anyone believes. A peer that already
/// learned this endpoint as that person's device still has; telling them is `device_revoke`, from
/// the device that holds the key.
pub(crate) async fn self_enroll_detach(
    state: &DaemonState,
) -> Result<mcpmesh_local_api::SelfEnrollDetachResult> {
    let mesh = state.mesh_required()?;
    // Serialized against `user_key_import` and `adopt_hook`, the other writers of this slot and
    // its sidecar.
    let _guard = mesh.user_key_lock.lock().await;
    let Some(adopted) = mesh
        .adopted_binding
        .read()
        .expect("adopted_binding lock not poisoned")
        .clone()
    else {
        let stale = remove_adopted_sidecar(mesh).await?;
        let path = mesh.adopted_binding_path();
        anyhow::bail!(crate::pairing::rendezvous::PairRefusal::new(
            mcpmesh_local_api::ERR_NOT_ENROLLED,
            if stale {
                format!(
                    "this device is not enrolled into another identity, but a stale enrollment \
                     file was left at {} (one boot declined, or an older daemon left) and has \
                     been removed, so the next boot will not re-adopt it",
                    path.display()
                )
            } else {
                "this device is not enrolled into another identity: there is no adopted binding \
                 to detach, and it already presents its own"
                    .into()
            },
        ));
    };
    remove_adopted_sidecar(mesh).await?;
    mesh.set_self_binding_live(None);
    // Audited like the adoption it undoes (`self_enroll_adopt`), keyed on the identity left.
    mesh.audit().record(AuditRecord::trust(
        now_ts(),
        "self_enroll_detach".into(),
        Some(adopted.user_pk.clone()),
        None,
    ));
    let user_id = mesh.self_binding().map(|b| b.user_pk);
    tracing::info!(
        detached_from = %adopted.user_pk,
        now = user_id.as_deref().unwrap_or("<no user key>"),
        "detached from an enrolled identity (#214)"
    );
    Ok(mcpmesh_local_api::SelfEnrollDetachResult {
        user_id,
        detached_from: adopted.user_pk,
    })
}

/// Remove the adopted-binding sidecar (#214). `Ok(true)` when a file was removed, `Ok(false)` when
/// there was none — a binding adopted live but never persisted (`adopt_hook` writes before it
/// installs, so that is a fixture, not a failed write) still detaches: what matters is what boot
/// will re-read, and nothing. Any other error is the caller's error.
async fn remove_adopted_sidecar(mesh: &Arc<MeshState>) -> Result<bool> {
    let sidecar = mesh.adopted_binding_path();
    blocking(
        "join adopted binding remove",
        move || match std::fs::remove_file(&sidecar) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(anyhow::anyhow!(
                "remove the adopted binding at {}: {e}",
                sidecar.display()
            )),
        },
    )
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::testutil::hermetic_mesh;

    /// #50: `unregister_service` removes the whole config entry (allow included), idempotently,
    /// without touching peer identity.
    #[tokio::test(flavor = "multi_thread")]
    async fn unregister_service_removes_the_entry_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = [\"eid:beef\"]\n             [services.notes]\nsocket = \"/run/notes.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let has = |name: &str| {
            crate::config::Config::load(&config_path)
                .unwrap()
                .services
                .contains_key(name)
        };

        assert!(has("kb") && has("notes"));
        unregister_service(&state, "kb".into()).await.unwrap();
        assert!(!has("kb"), "kb removed");
        assert!(has("notes"), "other services untouched");
        // Idempotent: unregistering an unknown / already-gone name is a clean no-op.
        unregister_service(&state, "kb".into()).await.unwrap();
        unregister_service(&state, "ghost".into()).await.unwrap();
        assert!(has("notes"));
    }

    /// #140: the two set differences must not be transposed.
    ///
    /// They are adjacent fields of the same type, and a reversed pair reads perfectly plausibly
    /// while telling a reporter the exact opposite of what is happening on their machine — "iroh
    /// has forgotten three of your hint's addresses" instead of "discovery found three your hint
    /// never had". Those lead to opposite conclusions about #140.
    ///
    /// The two inputs are deliberately DIFFERENT sizes with a shared element, so a transposed
    /// implementation cannot coincidentally agree.
    #[test]
    fn the_hint_and_iroh_differences_are_not_transposed() {
        let hint = vec![
            "192.168.1.5:4444".to_string(),
            "10.0.0.9:4444".to_string(),
            "relay https://r.example:443".to_string(),
        ];
        let known = vec![
            "192.168.1.5:4444".to_string(),  // in both
            "192.168.1.77:4444".to_string(), // discovery only
        ];
        let (unknown_to_iroh, not_in_hint) = super::addr_differences(&hint, &known);
        assert_eq!(
            unknown_to_iroh,
            vec!["10.0.0.9:4444", "relay https://r.example:443"],
            "these are in the stored hint and NOT in iroh's view"
        );
        assert_eq!(
            not_in_hint,
            vec!["192.168.1.77:4444"],
            "this is in iroh's view and NOT named by the hint"
        );
        // The shared address is in NEITHER difference. Without this the two lists could simply be
        // the two inputs and both assertions above would still hold.
        for d in [&unknown_to_iroh, &not_in_hint] {
            assert!(
                !d.contains(&"192.168.1.5:4444".to_string()),
                "an address in both lists belongs to neither difference: {d:?}"
            );
        }
    }

    /// Empty on either side is not a crash and not a swap: an empty hint means everything iroh
    /// knows is discovery's, and an empty iroh view means the whole hint is unknown to it. The
    /// second is the shape a stuck pairing is expected to show.
    #[test]
    fn one_sided_address_differences_land_on_the_correct_side() {
        let a = vec!["1.2.3.4:1".to_string()];
        assert_eq!(
            super::addr_differences(&[], &a),
            (vec![], a.clone()),
            "no hint: everything iroh holds came from somewhere else"
        );
        assert_eq!(
            super::addr_differences(&a, &[]),
            (a, vec![]),
            "an iroh entry that holds NO address really does leave the whole hint unheld — this is \
             the `Some([])` case. The `None` case never reaches here; the caller short-circuits it, \
             which is what the gate's CRITICAL finding was about."
        );
    }

    /// A relay URL is sanitized in the SHARED labeller, so `known_addrs` cannot leak a userinfo
    /// token that `hint_addrs` strips. This output exists to be pasted into a public issue, which
    /// makes it the highest-risk surface in the tree for that leak.
    #[test]
    fn a_relay_url_is_stripped_of_userinfo_wherever_it_is_labelled() {
        // A NON-default port, so the assertion proves the port is preserved as well as that the
        // credentials are gone. With `:443` the URL parser normalizes the port away and the test
        // could pass while dropping it.
        let url: iroh::RelayUrl = "https://tok3n:s3cret@relay.example:8443/"
            .parse::<url::Url>()
            .unwrap()
            .into();
        let labelled = super::label_transport_addr(&iroh::TransportAddr::Relay(url));
        assert_eq!(labelled, "relay https://relay.example:8443");
        for leak in ["s3cret", "tok3n", "@"] {
            assert!(
                !labelled.contains(leak),
                "a relay URL's userinfo must never reach a diagnostic meant for a public issue: \
                 {labelled}"
            );
        }
    }

    /// #140: the dump reports the durable state, and `hint_usable` agrees with what the DIAL
    /// actually does — including the case that is invisible from outside.
    ///
    /// A stored hint whose embedded id is not this peer, or that does not parse, is silently
    /// discarded by `stored_dial_addr` at EVERY dial: the node behaves as if it had no hint while
    /// the store says it has one. That is the discrepancy this verb exists to expose, so it is
    /// computed through `stored_dial_addr` itself rather than by re-reading the JSON — the two
    /// cannot disagree.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_diagnostics_reports_the_hint_the_dial_would_actually_use() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let key = iroh::SecretKey::from_bytes(&[3u8; 32]);
        let eid = *key.public().as_bytes();
        let other = *iroh::SecretKey::from_bytes(&[4u8; 32]).public().as_bytes();
        let seed = |last_addr: Option<String>| {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: eid,
                    nickname: "jetson".into(),
                    services: vec![],
                    paired_at: Some("1753000000".into()),
                    user_id: None,
                    last_addr,
                })
                .unwrap();
        };

        // No hint: the node dials by id alone — the same shape a freshly paired identity has, and
        // the baseline #140 compares against.
        seed(None);
        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        // #140 (api_minor 56): a peer this node has never dialled has NO iroh entry, reported as
        // `None` rather than flattened to an empty list — which still means something different
        // from "iroh has an entry and holds no address in it". (Absent does not mean "never heard
        // of": iroh also reaps a remote ~60s after its last connection closes.)
        assert_eq!(
            d.known_addrs, None,
            "an unknown peer must report no iroh ENTRY, not an empty address list"
        );
        assert!(d.hint_addrs_unknown_to_iroh.is_empty());
        assert!(d.iroh_addrs_not_in_hint.is_empty());
        assert_eq!(d.nickname, "jetson");
        assert!(d.principal.starts_with("eid:"), "{}", d.principal);
        assert_eq!(d.last_addr, None);
        assert!(!d.hint_usable, "no hint cannot be a usable hint");
        assert!(d.hint_addrs.is_empty());
        assert_eq!(d.paired_at.as_deref(), Some("1753000000"));

        // A usable hint: reported verbatim, with its addresses extracted.
        let good = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            key.public(),
            [iroh::TransportAddr::Ip(
                "192.168.1.50:4433".parse().unwrap(),
            )],
        ))
        .unwrap();
        seed(Some(good.clone()));
        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        assert_eq!(d.last_addr.as_deref(), Some(good.as_str()));
        assert!(d.hint_usable, "a well-formed hint for THIS peer is usable");
        assert_eq!(d.hint_addrs, vec!["192.168.1.50:4433".to_string()]);
        // **The gate's CRITICAL finding, inverted into a test.** With NO iroh entry there is
        // nothing to difference against, so BOTH lists must be empty. The first cut flattened
        // `None` to an empty view and reported the whole hint as "unknown to iroh" — on a daemon
        // that had simply never dialled the peer, which is the default state of the one the CLI
        // auto-starts. It printed "stale: 192.168.1.50:4433" about a hint that was current and
        // correct.
        assert_eq!(d.known_addrs, None, "precondition: iroh has no entry here");
        assert!(
            d.hint_addrs_unknown_to_iroh.is_empty(),
            "with no iroh view there is nothing to call stale — a current hint must not be \
             reported as absent from a view that does not exist: {:?}",
            d.hint_addrs_unknown_to_iroh
        );
        assert!(
            d.iroh_addrs_not_in_hint.is_empty(),
            "and nothing can be missing from the hint either: {:?}",
            d.iroh_addrs_not_in_hint
        );

        // The invisible case: a hint whose embedded id is a DIFFERENT peer. The store holds it,
        // and every dial throws it away. Reporting this as usable would send someone hunting the
        // wrong address.
        let mismatched = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            iroh::EndpointId::from_bytes(&other).unwrap(),
            [iroh::TransportAddr::Ip(
                "192.168.1.99:4433".parse().unwrap(),
            )],
        ))
        .unwrap();
        seed(Some(mismatched.clone()));
        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        assert_eq!(
            d.last_addr.as_deref(),
            Some(mismatched.as_str()),
            "the stored value is reported verbatim — that is the evidence"
        );
        assert!(
            !d.hint_usable,
            "a hint for a different endpoint is discarded at every dial; saying otherwise sends \
             the reader after an address the node never uses"
        );
        assert!(
            d.hint_addrs.is_empty(),
            "and its addresses are not this peer's"
        );

        // Garbage degrades the same way, rather than erroring the verb.
        seed(Some("not json at all".into()));
        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        assert!(!d.hint_usable);
        assert_eq!(d.last_addr.as_deref(), Some("not json at all"));

        // #140 gate: a RELAY-ONLY hint must be visible, not filtered into an empty line. This is
        // the shape #124 identified as harmful — it can never punch — and it is reachable in
        // production, because an invite minted while only the relay path was up stores exactly it.
        // Filtering to IP hid the single most diagnostic value the verb can report.
        let relay_only = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            key.public(),
            [iroh::TransportAddr::Relay(
                "https://user:token@relay.example/".parse().unwrap(),
            )],
        ))
        .unwrap();
        seed(Some(relay_only));
        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        assert_eq!(
            d.hint_addrs.len(),
            1,
            "the relay hint must be REPORTED: {d:?}"
        );
        assert!(d.hint_addrs[0].starts_with("relay "), "{:?}", d.hint_addrs);
        assert!(
            !d.hint_addrs[0].contains("token"),
            "a relay URL's userinfo must be SANITIZED — this output is meant to be pasted into an \
             issue, and every other surface sanitizes it: {:?}",
            d.hint_addrs
        );
    }

    /// #87: `max_uses` is clamped, `0` is rejected, and the CLAMPED value is what comes back.
    ///
    /// Reporting the requested value rather than the applied one would let a caller ask for 500,
    /// be told 500, and discover the truth when the 65th colleague fails. Rejecting `0` rather
    /// than treating it as "unusable" surfaces a caller bug instead of minting a credential
    /// nobody can redeem.
    /// #65: the endorsement path, end to end through the handler.
    ///
    /// Property 2 — **an introduction grants nothing** — is what bounds the whole feature, so it is
    /// asserted on the stored row rather than inferred from the absence of a grant call.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_introduction_installs_identity_and_grants_nothing() {
        use mcpmesh_local_api::PeerIntroduceParams;
        use mcpmesh_trust::keys::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // Carol: someone we ALREADY paired with, so her user_id is stored.
        let (carol, _) = UserKey::load_or_generate(&dir.path().join("carol.key")).unwrap();
        let carol_uid = mcpmesh_trust::binding::user_id(&carol);
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: [0xC0; 32],
                nickname: "carol".into(),
                services: vec!["notes".into()],
                paired_at: Some("1".into()),
                user_id: Some(carol_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        // Endpoint ids must be VALID ed25519 points — an arbitrary [0xAA; 32] is not one, and
        // `EndpointId::from_bytes` rightly refuses it.
        let eid_from = |seed: u8| -> ([u8; 32], String) {
            let pk = iroh::SecretKey::from_bytes(&[seed; 32]).public();
            (*pk.as_bytes(), pk.to_string())
        };
        let (bob_eid, bob_str) = eid_from(0xBB);
        let evidence = mcpmesh_trust::binding::endorse(&carol, &bob_eid, None).unwrap();
        let params = |endorsed_by: &str, evidence: &str, nickname: &str| PeerIntroduceParams {
            subject: bob_str.clone(),
            endorsed_by: endorsed_by.to_string(),
            evidence: evidence.to_string(),
            subject_user_id: None,
            subject_binding: None,
            nickname: nickname.to_string(),
        };

        introduce_peer(&state, params(&carol_uid, &evidence, "bob"))
            .await
            .expect("a valid endorsement from a paired peer installs the subject");

        let bob = mesh
            .store
            .resolve(&bob_eid)
            .unwrap()
            .expect("the subject is now resolvable");
        assert_eq!(bob.nickname, "bob");
        assert!(
            bob.services.is_empty(),
            "AN INTRODUCTION MUST GRANT NOTHING — this is the property that bounds the whole \
             feature: a compromised endorser can make us KNOW about a peer, never SERVE it. Got: \
             {:?}",
            bob.services
        );
        assert_eq!(
            bob.paired_at, None,
            "no SAS happened, so nothing may claim a pairing stamp"
        );

        // A stranger's endorsement — valid signature, unknown endorser — is refused.
        let (mallory, _) = UserKey::load_or_generate(&dir.path().join("m.key")).unwrap();
        let m_uid = mcpmesh_trust::binding::user_id(&mallory);
        let m_ev = mcpmesh_trust::binding::endorse(&mallory, &eid_from(0xEE).0, None).unwrap();
        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: eid_from(0xEE).1,
                endorsed_by: m_uid,
                evidence: m_ev,
                subject_user_id: None,
                subject_binding: None,
                nickname: "eve".into(),
            },
        )
        .await
        .expect_err("an endorsement from a peer we never paired with must be refused");
        assert!(
            format!("{e:#}").contains("currently paired"),
            "and say why: {e:#}"
        );

        // A signature for a DIFFERENT subject does not transplant.
        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: eid_from(0xAA).1,
                endorsed_by: carol_uid.clone(),
                evidence: evidence.clone(),
                subject_user_id: None,
                subject_binding: None,
                nickname: "someone".into(),
            },
        )
        .await
        .expect_err("an endorsement naming bob must not install someone else");
        assert!(format!("{e:#}").contains("does not verify"), "{e:#}");

        // Our OWN endpoint id is refused.
        let ours = mesh.endpoint.id().to_string();
        let self_ev =
            mcpmesh_trust::binding::endorse(&carol, mesh.endpoint.id().as_bytes(), None).unwrap();
        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: ours,
                endorsed_by: carol_uid.clone(),
                evidence: self_ev,
                subject_user_id: None,
                subject_binding: None,
                nickname: "me".into(),
            },
        )
        .await
        .expect_err("introducing ourselves must be refused");
        assert!(format!("{e:#}").contains("own endpoint id"), "{e:#}");

        // The nickname goes through the SAME validation as every other stored name (#87) — a `/`
        // would make every mount of that peer unparseable, and the collision test alone does not
        // pin it: deleting the `validated_alias` call passed the whole suite.
        for bad in ["with/slash", "", "   "] {
            let ev_bad = mcpmesh_trust::binding::endorse(&carol, &eid_from(0xAB).0, None).unwrap();
            let e = introduce_peer(
                &state,
                PeerIntroduceParams {
                    subject: eid_from(0xAB).1,
                    endorsed_by: carol_uid.clone(),
                    evidence: ev_bad,
                    subject_user_id: None,
                    subject_binding: None,
                    nickname: bad.to_string(),
                },
            )
            .await
            .unwrap_err();
            assert!(
                format!("{e:#}").contains("nickname"),
                "a {bad:?} nickname must be refused by the shared validator: {e:#}"
            );
        }

        // A colliding nickname is refused, exactly as pairing refuses it.
        let ev2 = mcpmesh_trust::binding::endorse(&carol, &eid_from(0xDD).0, None).unwrap();
        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: eid_from(0xDD).1,
                endorsed_by: carol_uid.clone(),
                evidence: ev2,
                subject_user_id: None,
                subject_binding: None,
                nickname: "carol".into(),
            },
        )
        .await
        .expect_err("a nickname already used for a different peer must be refused");
        assert!(format!("{e:#}").contains("already use the name"), "{e:#}");
    }

    /// #65 gate, THE exploit: an endorser must not be able to hand the subject someone else's
    /// `user_id`.
    ///
    /// `PeerEntry.services` is NOT the authorization input — `user_id` is, because
    /// `[services.*].allow` matches on it. The first version let the ENDORSER assert it, and a
    /// `user_id` is public (it is on `status`, on `PairResult`, on every audit record). So a
    /// compromised endorser could endorse an ATTACKER's endpoint carrying a VICTIM's user_id, and
    /// the attacker inherited that victim's grants — the exact inverse of this feature's claimed
    /// bound, demonstrated end to end in review.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_endorser_cannot_hand_the_subject_someone_elses_user_id() {
        use mcpmesh_local_api::PeerIntroduceParams;
        use mcpmesh_trust::keys::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (carol, _) = UserKey::load_or_generate(&dir.path().join("carol.key")).unwrap();
        let carol_uid = mcpmesh_trust::binding::user_id(&carol);
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: [0xC0; 32],
                nickname: "carol".into(),
                services: vec![],
                paired_at: Some("1".into()),
                user_id: Some(carol_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        // Alice is a real, granted person. Her user_id is PUBLIC.
        let (alice, _) = UserKey::load_or_generate(&dir.path().join("alice.key")).unwrap();
        let alice_uid = mcpmesh_trust::binding::user_id(&alice);

        // Mallory's own endpoint. Carol signs a perfectly valid endorsement of it — carrying
        // ALICE's user_id.
        let mallory_pk = iroh::SecretKey::from_bytes(&[0x4D; 32]).public();
        let mallory_eid = *mallory_pk.as_bytes();
        let ev = mcpmesh_trust::binding::endorse(&carol, &mallory_eid, Some(&alice_uid)).unwrap();

        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: mallory_pk.to_string(),
                endorsed_by: carol_uid.clone(),
                evidence: ev.clone(),
                subject_user_id: Some(alice_uid.clone()),
                subject_binding: None,
                nickname: "mallory".into(),
            },
        )
        .await
        .expect_err("a user_id vouched for by the ENDORSER alone must be refused");
        assert!(
            format!("{e:#}").contains("subject_binding"),
            "and say the subject must prove it: {e:#}"
        );

        // …and Mallory cannot supply Alice's binding either: she does not hold Alice's key, and a
        // binding is bound to the endpoint, so nothing she can produce verifies.
        let forged = mcpmesh_trust::binding::present(&alice, &mallory_eid).1;
        let ok = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: mallory_pk.to_string(),
                endorsed_by: carol_uid.clone(),
                evidence: ev,
                subject_user_id: Some(alice_uid.clone()),
                subject_binding: Some(forged),
                nickname: "mallory".into(),
            },
        )
        .await;
        // This one DOES verify — it is Alice's own key signing Mallory's endpoint, which only
        // Alice could produce. That is the correct semantics: possession of the user key is what
        // the binding proves, and a test must not pretend otherwise.
        assert!(
            ok.is_ok(),
            "a binding Alice herself signed is valid by construction: {ok:?}"
        );

        // The property that actually matters: an endorser WITHOUT the victim's key cannot do it.
        let mallory2 = iroh::SecretKey::from_bytes(&[0x4E; 32]).public();
        let ev2 =
            mcpmesh_trust::binding::endorse(&carol, mallory2.as_bytes(), Some(&alice_uid)).unwrap();
        let carol_forgery = mcpmesh_trust::binding::present(&carol, mallory2.as_bytes()).1;
        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: mallory2.to_string(),
                endorsed_by: carol_uid.clone(),
                evidence: ev2,
                subject_user_id: Some(alice_uid),
                subject_binding: Some(carol_forgery),
                nickname: "mallory2".into(),
            },
        )
        .await
        .expect_err("a binding signed by the ENDORSER's key cannot vouch for the VICTIM's user_id");
        assert!(format!("{e:#}").contains("does not verify"), "{e:#}");
    }

    /// #65 gate: introductions must NOT chain. Without requiring `paired_at` on the endorser, an
    /// introduced peer becomes an endorser as soon as it has a user_id, so the chain reaches
    /// unbounded depth and terminates at no ceremony the operator ever performed.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_introduced_peer_cannot_introduce_others() {
        use mcpmesh_local_api::PeerIntroduceParams;
        use mcpmesh_trust::keys::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (carol, _) = UserKey::load_or_generate(&dir.path().join("carol.key")).unwrap();
        let carol_uid = mcpmesh_trust::binding::user_id(&carol);
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: [0xC0; 32],
                nickname: "carol".into(),
                services: vec![],
                paired_at: Some("1".into()),
                user_id: Some(carol_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        // Carol introduces Bob, WITH a proven user_id (Bob signs his own binding).
        let (bobkey, _) = UserKey::load_or_generate(&dir.path().join("bob.key")).unwrap();
        let bob_uid = mcpmesh_trust::binding::user_id(&bobkey);
        let bob_pk = iroh::SecretKey::from_bytes(&[0xB0; 32]).public();
        let bob_eid = *bob_pk.as_bytes();
        introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: bob_pk.to_string(),
                endorsed_by: carol_uid,
                evidence: mcpmesh_trust::binding::endorse(&carol, &bob_eid, Some(&bob_uid))
                    .unwrap(),
                subject_user_id: Some(bob_uid.clone()),
                subject_binding: Some(mcpmesh_trust::binding::present(&bobkey, &bob_eid).1),
                nickname: "bob".into(),
            },
        )
        .await
        .expect("carol is paired, so her endorsement installs bob");

        // The row must actually CARRY the proven user_id. Asserting only the chain refusal below
        // passes for the wrong reason: with `user_id: None` Bob still fails the endorser check and
        // still produces the same error string. Mutating the write to `None` survived the whole
        // suite before this assertion existed.
        let bob_row = mesh
            .store
            .resolve(&bob_eid)
            .unwrap()
            .expect("bob is installed");
        assert_eq!(
            bob_row.user_id.as_deref(),
            Some(bob_uid.as_str()),
            "a user_id the SUBJECT proved must be written — dropping it silently loses \
             multi-device resolution, which is the reason the field exists"
        );

        // Bob now has a user_id — but was never paired. He must NOT be able to endorse.
        let dave_pk = iroh::SecretKey::from_bytes(&[0xDA; 32]).public();
        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: dave_pk.to_string(),
                endorsed_by: bob_uid,
                evidence: mcpmesh_trust::binding::endorse(&bobkey, dave_pk.as_bytes(), None)
                    .unwrap(),
                subject_user_id: None,
                subject_binding: None,
                nickname: "dave".into(),
            },
        )
        .await
        .expect_err("an INTRODUCED peer must not be able to introduce others");
        assert!(
            format!("{e:#}").contains("currently paired"),
            "the endorser check must require a PAIRING, not merely a stored user_id — otherwise \
             introductions chain to unbounded depth: {e:#}"
        );
    }

    /// #65 gate: the PRODUCE half had no handler test at all — only its dispatch arm and serde tag
    /// were pinned, so signing the wrong subject entirely passed the suite.
    #[tokio::test(flavor = "multi_thread")]
    async fn endorse_peer_signs_the_subject_it_was_asked_about() {
        use mcpmesh_local_api::PeerEndorseParams;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        // On disk first, as boot leaves it: `peer_endorse` reads the key and never mints one (#221).
        let (own, _) = mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        mesh.set_user_key_path(key_path.clone());
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let subject = iroh::SecretKey::from_bytes(&[0x77; 32]).public();
        let res = endorse_peer(
            &state,
            PeerEndorseParams {
                subject: subject.to_string(),
                subject_user_id: None,
            },
        )
        .await
        .expect("endorsing produces evidence");

        // The evidence must verify FOR THIS SUBJECT under the returned endorser id — signing a
        // placeholder, or the wrong subject, would still return a well-formed pair.
        mcpmesh_trust::binding::verify_endorsement(
            &res.endorsed_by,
            &res.evidence,
            subject.as_bytes(),
            None,
        )
        .expect("the evidence must verify for the subject we asked about");

        // …and NOT for a different one.
        let other = iroh::SecretKey::from_bytes(&[0x78; 32]).public();
        assert!(
            mcpmesh_trust::binding::verify_endorsement(
                &res.endorsed_by,
                &res.evidence,
                other.as_bytes(),
                None,
            )
            .is_err(),
            "the signature must name the subject, not merely be well-formed"
        );

        // `endorsed_by` must be OUR user id, so a recipient paired with us can resolve it.
        assert_eq!(res.endorsed_by, mcpmesh_trust::binding::user_id(&own));

        // Endorsing must change NO local trust state — it is a statement for someone else.
        assert!(
            mesh.store.list().unwrap().is_empty(),
            "endorsing a peer must not install it locally"
        );

        // A malformed subject is a clean params error, not a signature over garbage.
        endorse_peer(
            &state,
            PeerEndorseParams {
                subject: "not-an-endpoint-id".into(),
                subject_user_id: None,
            },
        )
        .await
        .expect_err("a malformed subject must be refused");
    }

    /// #65 gate, HIGH: an endorsement must not outlive the unpairing that ends the relationship.
    ///
    /// Carol is paired and granted via her `user_id`. She self-endorses a SECOND endpoint and has
    /// it installed. Unpairing Carol used to leave her `user_id` in every allow list — the strip is
    /// skipped when the id looks "shared with another device" — and `store.remove` is
    /// nickname-scoped, so the second row survived with full access, forever, from one pasted
    /// payload she wrote herself.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_introduction_cannot_outlive_the_unpairing_of_its_endorser() {
        use mcpmesh_local_api::PeerIntroduceParams;
        use mcpmesh_trust::keys::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let (carol, _) = UserKey::load_or_generate(&dir.path().join("carol.key")).unwrap();
        let carol_uid = mcpmesh_trust::binding::user_id(&carol);
        std::fs::write(
            &config_path,
            format!("[services.notes]\nsocket = \"/run/n.sock\"\nallow = [\"{carol_uid}\"]\n"),
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: [0xC0; 32],
                nickname: "carol".into(),
                services: vec!["notes".into()],
                paired_at: Some("1".into()),
                user_id: Some(carol_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        // Carol's SECOND endpoint, endorsed by Carol herself, with her own proven binding.
        let second = iroh::SecretKey::from_bytes(&[0xC1; 32]).public();
        introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: second.to_string(),
                endorsed_by: carol_uid.clone(),
                evidence: mcpmesh_trust::binding::endorse(
                    &carol,
                    second.as_bytes(),
                    Some(&carol_uid),
                )
                .unwrap(),
                subject_user_id: Some(carol_uid.clone()),
                subject_binding: Some(mcpmesh_trust::binding::present(&carol, second.as_bytes()).1),
                nickname: "carols-other-laptop".into(),
            },
        )
        .await
        .expect("carol is paired, so her self-endorsement installs");

        // Now unpair Carol.
        remove_peer(
            &state,
            mcpmesh_local_api::PeerRemoveParams {
                nickname: "carol".into(),
            },
        )
        .await
        .expect("unpair succeeds");

        let cfg = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            !cfg.contains(&carol_uid),
            "unpairing must STRIP the grant even though an INTRODUCED row still shares the \
             user_id — an introduced row is not a device you vouched for, so counting it as \
             'still trusted' lets one pasted endorsement survive the unpairing forever. Config \
             still reads:\n{cfg}"
        );
    }

    /// #65 gate: the refusals and the audit record this round ADDED, each pinned.
    ///
    /// All three survived the suite when first written — including the `already_paired` refusal and
    /// the audit record, which were themselves fixes for review findings.
    #[tokio::test(flavor = "multi_thread")]
    async fn introduce_refuses_the_shapes_it_claims_to_and_audits_the_write() {
        use mcpmesh_local_api::PeerIntroduceParams;
        use mcpmesh_trust::keys::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let audit =
            crate::audit::AuditSink::new(crate::audit::AuditLog::spawn(dir.path().join("audit")));
        mesh.set_audit(audit.clone());
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (carol, _) = UserKey::load_or_generate(&dir.path().join("carol.key")).unwrap();
        let carol_uid = mcpmesh_trust::binding::user_id(&carol);
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: [0xC0; 32],
                nickname: "carol".into(),
                services: vec![],
                paired_at: Some("1".into()),
                user_id: Some(carol_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        let subj = iroh::SecretKey::from_bytes(&[0x51; 32]).public();
        let ev = mcpmesh_trust::binding::endorse(&carol, subj.as_bytes(), None).unwrap();
        let base = |uid: Option<String>, bind: Option<String>| PeerIntroduceParams {
            subject: subj.to_string(),
            endorsed_by: carol_uid.clone(),
            evidence: ev.clone(),
            subject_user_id: uid,
            subject_binding: bind,
            nickname: "subj".into(),
        };

        // A binding with nothing to bind is refused, not ignored.
        let e = introduce_peer(&state, base(None, Some("b64u:whatever".into())))
            .await
            .expect_err("subject_binding without subject_user_id must be refused");
        assert!(format!("{e:#}").contains("nothing to bind"), "{e:#}");

        // The happy path, then the AUDIT record it must leave. Read off the live ring rather than
        // the file, so the assertion does not race the writer's flush.
        let mut rx = audit.subscribe().expect("auditing enabled");
        introduce_peer(&state, base(None, None)).await.unwrap();
        let mut recs = Vec::new();
        while let Ok(r) = rx.try_recv() {
            recs.push(r);
        }
        let rec = recs
            .iter()
            .find(|r| r.event.as_deref() == Some("peer_introduce"))
            .expect(
                "a trust-establishing write with NO human ceremony is exactly the one an operator \
                 needs a record of — pair/unpair/roster_install all emit one",
            );
        assert_eq!(rec.target.as_deref(), Some("subj"));
        assert_eq!(
            rec.principal.as_deref(),
            Some(carol_uid.as_str()),
            "the record must name the ENDORSER — the question anyone reading it will have"
        );

        // An ALREADY PAIRED peer must not be overwritten by a weaker row.
        let paired = iroh::SecretKey::from_bytes(&[0x52; 32]).public();
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: *paired.as_bytes(),
                nickname: "dana".into(),
                services: vec![],
                paired_at: Some("7".into()),
                user_id: None,
                last_addr: Some("hint".into()),
            })
            .unwrap();
        let ev2 = mcpmesh_trust::binding::endorse(&carol, paired.as_bytes(), None).unwrap();
        let e = introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: paired.to_string(),
                endorsed_by: carol_uid.clone(),
                evidence: ev2,
                subject_user_id: None,
                subject_binding: None,
                nickname: "dana2".into(),
            },
        )
        .await
        .expect_err("introducing an already-PAIRED peer must be refused");
        assert!(format!("{e:#}").contains("already paired"), "{e:#}");
        assert_eq!(
            mesh.store
                .resolve(paired.as_bytes())
                .unwrap()
                .unwrap()
                .last_addr,
            Some("hint".into()),
            "and the SAS-proven row must be untouched — an upsert would have destroyed its hint"
        );
    }

    /// #65: the chain must be LIVE — unpairing the endorser revokes their power to introduce.
    #[tokio::test(flavor = "multi_thread")]
    async fn unpairing_the_endorser_revokes_their_introductions() {
        use mcpmesh_local_api::PeerIntroduceParams;
        use mcpmesh_trust::keys::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (carol, _) = UserKey::load_or_generate(&dir.path().join("carol.key")).unwrap();
        let carol_uid = mcpmesh_trust::binding::user_id(&carol);
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: [0xC0; 32],
                nickname: "carol".into(),
                services: vec![],
                paired_at: Some("1".into()),
                user_id: Some(carol_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        let bob_pk = iroh::SecretKey::from_bytes(&[0xBB; 32]).public();
        let ev = mcpmesh_trust::binding::endorse(&carol, bob_pk.as_bytes(), None).unwrap();
        let p = |n: &str| PeerIntroduceParams {
            subject: bob_pk.to_string(),
            endorsed_by: carol_uid.clone(),
            evidence: ev.clone(),
            subject_user_id: None,
            subject_binding: None,
            nickname: n.to_string(),
        };
        introduce_peer(&state, p("bob"))
            .await
            .expect("works while paired");

        // Unpair Carol. Her old, still-valid signature must stop working.
        mesh.store.remove("carol").unwrap();
        let e = introduce_peer(&state, p("bob2"))
            .await
            .expect_err("an endorsement from an UNPAIRED peer must be refused");
        assert!(
            format!("{e:#}").contains("currently paired"),
            "the check must be on the CURRENT store, not on whether the signature is valid — a \
             signature stays valid forever, which is exactly why the chain has to be live: {e:#}"
        );
    }

    /// #63: the control path must go through the SAME clamp as config, and `0` must be refused.
    ///
    /// An unclamped `rate_limit_per_min` on `register_service` is what got the first attempt at
    /// this parked: one control call uncapped a service, where `[limits].rate_limit_per_min` had
    /// previously been a hard ceiling no control call could raise.
    #[tokio::test(flavor = "multi_thread")]
    async fn register_service_cannot_uncap_a_service() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[limits]\nrate_limit_per_min = 5\n").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        // MUST be installed explicitly: `MeshState::limits()` FAILS OPEN — a OnceCell miss returns
        // `unlimited()`, so a test mesh silently has no rate limits at all and every assertion
        // about clamping would be vacuously about an unlimited bundle.
        mesh.set_limits(crate::limits::MeshLimiters::from_config(
            &crate::config::LimitsCfg {
                rate_limit_per_min: 5,
                ..Default::default()
            },
        ));
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let params = |rate: Option<u32>| RegisterServiceParams {
            name: "svc".into(),
            backend: mcpmesh_local_api::BackendSpec::Socket {
                path: "/run/svc.sock".into(),
            },
            allow: vec![],
            ephemeral: true,
            rate_limit_per_min: rate,
        };

        // `0` is refused rather than silently blocking every request.
        let e = register_service(&state, params(Some(0)))
            .await
            .expect_err("rate_limit_per_min = 0 must be refused");
        assert!(
            format!("{e:#}").contains("at least 1"),
            "and say what a valid value is: {e:#}"
        );

        // A BELOW-ceiling rate must actually reach the ephemeral backend's bucket. Asserting on
        // `effective_rpm` alone would be arithmetic, not wiring — and the ephemeral path silently
        // dropping a per-service feature is exactly the shape #55 was filed about.
        register_service(&state, params(Some(2)))
            .await
            .expect("a below-ceiling rate registers");
        assert_eq!(
            mesh.limits().tracked_rpm("svc"),
            Some(Some(2)),
            "an EPHEMERAL registration's rate must reach its backend's bucket — dropping it here \
             is the #55 shape: a per-service feature that silently does nothing for ephemerals"
        );

        // The PERSISTENT path (`ephemeral: false`, which is the DEFAULT) must carry the rate too.
        // Passing `None` to `write_service_to_config` there passed the whole workspace: the new
        // tests used `ephemeral: true` exclusively, and the config-writer test called the writer
        // directly without crossing the handler. One path over from the #55 shape again.
        let persistent = RegisterServiceParams {
            name: "persisted".into(),
            backend: mcpmesh_local_api::BackendSpec::Socket {
                path: "/run/p.sock".into(),
            },
            allow: vec![],
            ephemeral: false,
            rate_limit_per_min: Some(3),
        };
        register_service(&state, persistent)
            .await
            .expect("a persistent registration with a rate succeeds");
        assert_eq!(
            mesh.limits().tracked_rpm("persisted"),
            Some(Some(3)),
            "a PERSISTENT registration's rate must survive the config write and the reload — \
             `ephemeral` defaults to false, so this is the default path"
        );

        // A wildly-over-ceiling request is ACCEPTED but CLAMPED — again through the real bucket.
        register_service(&state, params(Some(1_000_000)))
            .await
            .expect("an over-ceiling rate is clamped, not rejected");
        assert_eq!(
            mesh.limits().tracked_rpm("svc"),
            Some(Some(5)),
            "the control path must clamp to [limits].rate_limit_per_min exactly as config does — \
             one call must never be able to uncap a service"
        );
    }

    /// #87 gate: pin the CALL SITE, not just the validator.
    ///
    /// A helper test proves nothing about whether `redeem` calls it — deleting the call passed the
    /// entire workspace. Driven with a deliberately GARBAGE invite line: validation runs before the
    /// line is even decoded, so an alias error here proves the order as well as the call.
    #[tokio::test(flavor = "multi_thread")]
    async fn redeem_validates_as_nickname_before_it_touches_the_invite_line() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh);

        let e = redeem(
            &state,
            "total-garbage-not-an-invite".into(),
            Some("  ".into()),
            crate::pairing::SelfEnroll::Refuse,
        )
        .await
        .expect_err("a blank as_nickname must be refused");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("as_nickname") && msg.contains("empty"),
            "the ALIAS error must win over the invite-decode error — proving validation runs at \
             this call site, and runs FIRST: {msg}"
        );

        let e = redeem(
            &state,
            "total-garbage-not-an-invite".into(),
            Some("a/b".into()),
            crate::pairing::SelfEnroll::Refuse,
        )
        .await
        .expect_err("a '/' in as_nickname must be refused");
        assert!(format!("{e:#}").contains("as_nickname"), "{e:#}");

        // …and a VALID alias must fall through to the real work (which fails on the garbage line).
        let e = redeem(
            &state,
            "total-garbage".into(),
            Some("fine".into()),
            crate::pairing::SelfEnroll::Refuse,
        )
        .await
        .expect_err("the garbage line still fails");
        assert!(
            !format!("{e:#}").contains("as_nickname"),
            "a valid alias must not be reported as an alias problem: {e:#}"
        );
    }

    /// #87 gate: BOTH aliases go through the same validation, and `as_nickname`'s branch had no
    /// test at all — making a blank one fall back silently (the exact behaviour the comment forbids)
    /// passed the whole suite.
    ///
    /// The rules match `set_nickname`'s because the value lands in the same place: a stored
    /// nickname, which is the `<peer>/<service>` mount prefix. Shipping them unvalidated let
    /// `"alice/notes"` make a peer permanently unmountable, and `" alice "` slip past a collision
    /// check that compares exact bytes while rendering identically to `alice` in any trimming UI.
    #[test]
    fn an_alias_is_validated_the_same_way_a_nickname_is() {
        for field in ["as_nickname", "peer_nickname"] {
            assert_eq!(
                validated_alias(field, None).unwrap(),
                None,
                "absent is fine"
            );
            assert_eq!(
                validated_alias(field, Some("  alice  ".into())).unwrap(),
                Some("alice".into()),
                "a valid alias is TRIMMED — otherwise ' alice ' evades the exact-byte collision \
                 check and renders as a duplicate of 'alice'"
            );

            for bad in ["", "   ", "\t\n"] {
                let e = validated_alias(field, Some(bad.into()))
                    .unwrap_err()
                    .to_string();
                assert!(
                    e.contains(field) && e.contains("empty"),
                    "blank must be a clean error naming the field, never a silent fallback to the \
                     name the caller was trying to avoid: {e}"
                );
            }
            let e = validated_alias(field, Some("alice/notes".into()))
                .unwrap_err()
                .to_string();
            assert!(
                e.contains('/') && e.contains(field),
                "'/' must be refused — the porcelain splits <peer>/<service> at the first one, so \
                 the peer would be permanently unmountable: {e}"
            );
            validated_alias(field, Some("line1\nline2".into()))
                .expect_err("control characters must be refused");
            validated_alias(field, Some("a".repeat(MAX_ALIAS_CHARS + 1)))
                .expect_err("an over-long alias must be refused");
            validated_alias(field, Some("a".repeat(MAX_ALIAS_CHARS)))
                .expect("exactly the cap is allowed — both sides of the boundary");
        }
    }

    /// #86 gate: an ENROLLED device must not be able to endorse (#65) or enroll a third device.
    ///
    /// The commit claimed "an enrolled device cannot enroll a third — it holds no private key".
    /// That was FALSE: boot always mints a local user key, so the device signed with the LOCAL key
    /// while PRESENTING the adopted one — producing endorsements under an id no peer has ever
    /// #85 ask 2: a machine that just RECOVERED its identity must still be able to endorse and to
    /// enroll its owner's other devices.
    ///
    /// The first version of `user_key_import` installed the restored binding through #86's
    /// `adopted_binding` slot. But that slot does not mean "the binding to present" — it means
    /// *this device was enrolled into someone else's identity and holds no authority over it*, and
    /// `peer_endorse` and `sign_binding` both gate on exactly that reading.
    ///
    /// So importing your own key made the node refuse to endorse, with a message saying it did not
    /// hold a key it had just imported — and silently disabled `invite --as-self`, which is the
    /// very remedy the recovery CLI prints ("enroll this device from another one you still hold").
    /// A recovered person could not re-enroll their other devices FROM the recovered machine.
    ///
    /// Review found it by probe. The fix gives the import its own slot; this pins that the two
    /// questions stay separate.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_recovered_device_can_still_endorse_and_enroll() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        mesh.set_user_key_path(key_path.clone());
        // Model a node whose key was LOADED, not minted this boot — the case the replace guard
        // defends, and the one an import has to survive.
        let (existing, _) = mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        mesh.note_user_key_minted_at_boot(false);
        // …and PRESENTS it, as boot does for a key it loads. `sign_binding` refuses when the
        // identity it would sign for is not the one presented (#221).
        let (user_pk, sig) =
            mcpmesh_trust::binding::present(&existing, mesh.endpoint.id().as_bytes());
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk,
            sig,
        }));
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // Precondition: before the import this device holds its own key and can enroll.
        assert!(
            (mesh.inviter_ctx().sign_binding)([7u8; 32]).await.is_some(),
            "precondition: a device holding its own key can sign a device binding"
        );

        let phrase = crate::pairing::recovery::encode(&existing.signing_key().to_bytes());
        let out = user_key_import(&state, phrase, true)
            .await
            .expect("import succeeds");
        assert_eq!(out.user_id, mcpmesh_trust::binding::user_id(&existing));

        // THE assertion. Both of these went false when the import borrowed #86's slot.
        assert!(
            mesh.adopted_binding.read().expect("lock").is_none(),
            "an import must not mark this device as ENROLLED — it holds the key it just imported"
        );
        assert!(
            (mesh.inviter_ctx().sign_binding)([7u8; 32]).await.is_some(),
            "…so `invite --as-self` must still be able to enroll another device, which is exactly \
             the remedy the recovery CLI tells a recovered person to use"
        );

        // And the restored binding IS the one presented.
        let presented = mesh.self_binding().expect("a binding is presented");
        assert_eq!(
            presented.user_pk,
            mcpmesh_trust::roster::encode_b64u(&existing.public_bytes()),
            "the presented identity must be the restored one"
        );
    }

    /// paired with, silently unredeemable.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_enrolled_device_cannot_endorse_or_enroll() {
        use mcpmesh_local_api::PeerEndorseParams;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        // The key is on disk BEFORE anything runs, as boot leaves it — `peer_endorse` reads it and
        // no longer mints one (#221).
        mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        mesh.set_user_key_path(key_path);
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let subject = iroh::SecretKey::from_bytes(&[0x91; 32]).public();

        // Before enrollment: endorsing works.
        endorse_peer(
            &state,
            PeerEndorseParams {
                subject: subject.to_string(),
                subject_user_id: None,
            },
        )
        .await
        .expect("a device holding its own key can endorse");

        // Enroll this device into someone else's identity.
        let adopted = crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:someone-elses".into(),
            sig: "b64u:sig".into(),
        };
        mesh.set_self_binding_live(Some(adopted.clone()));

        let e = endorse_peer(
            &state,
            PeerEndorseParams {
                subject: subject.to_string(),
                subject_user_id: None,
            },
        )
        .await
        .expect_err("an enrolled device must refuse to endorse");
        assert!(
            format!("{e:#}").contains("does not hold that user key"),
            "and say WHY, since the caller's request looks reasonable: {e:#}"
        );

        // …and it must refuse to sign enrollment bindings for a third device.
        assert!(
            (mesh.inviter_ctx().sign_binding)(*subject.as_bytes())
                .await
                .is_none(),
            "an enrolled device must not sign a binding for a THIRD device — it would be for an \
             identity no peer has seen"
        );
        // …refused by the #86 GATE. The key check behind it would also refuse (the local key is
        // not "someone-elses"), so without this a sign_binding with no gate at all passed here.
        assert_eq!(
            mesh.sign_ceremony_binding(Some(&adopted), subject.as_bytes())
                .await,
            Err(crate::daemon::SignBindingRefusal::Enrolled)
        );
    }

    /// #219: `user_key_export` on an ENROLLED device must refuse, not hand back the LOCAL key.
    ///
    /// Boot always mints a local user key. On a device enrolled into someone else's identity
    /// (#86) that key backs an identity no peer has ever paired with, and its `user_id` is not the
    /// one the node presents. A person "backing up their identity" on that laptop would write down
    /// the wrong phrase — and only find out on the new hardware, as a stranger to every peer.
    ///
    /// The same gate `peer_endorse` uses, checked BEFORE the key is read.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_enrolled_device_refuses_to_export_the_local_key() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        mesh.set_user_key_path(key_path.clone());
        // The boot-minted LOCAL key and the binding derived from it — what an un-gated export
        // would happily hand back.
        let (local, _) = mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        let local_id = mcpmesh_trust::binding::user_id(&local);
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // Precondition: before enrollment the export works and names the local key, so the
        // refusal below is the gate and not a broken fixture.
        let before = user_key_export(&state)
            .await
            .expect("a device holding its own key exports it");
        assert_eq!(before.user_id, local_id);

        // Enroll this device into someone else's identity through the REAL adopt hook — the
        // path a `mcpmesh-enroll:` redemption takes — so `adopted_binding` is set the way it is
        // on a real enrolled device, and the key file still sits underneath.
        adopt_hook(&mesh)(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:someone-elses-identity".into(),
            sig: "b64u:sig".into(),
        })
        .await
        .expect("the hook persists and installs");
        assert!(
            key_path.exists(),
            "the local boot key is still on disk underneath"
        );
        assert_ne!(
            mesh.self_binding().expect("a binding").user_pk,
            mcpmesh_trust::roster::encode_b64u(&local.public_bytes()),
            "fixture: the presented identity is NOT the local key's"
        );

        // THE assertion: refused, with no phrase and no user_id in the answer.
        let e = match user_key_export(&state).await {
            Ok(out) => panic!(
                "an enrolled device must refuse to export — it returned a phrase for {} instead \
                 (the local key no peer has paired with)",
                out.user_id
            ),
            Err(e) => e,
        };
        assert!(
            e.downcast_ref::<crate::control::InvalidParams>().is_some(),
            "the refusal must be the coded (-32602) one peer_endorse uses, not a bare failure: {e:#}"
        );
        let msg = format!("{e:#}");
        assert!(
            msg.contains("does not hold that user key") && msg.contains("#86"),
            "and say WHY, in the same shape as peer_endorse: {msg}"
        );
        assert!(
            !msg.contains(&local_id),
            "the refusal must not leak the local user_id either: {msg}"
        );
    }

    /// #221: the `replace` guard must protect an IMPORTED key, not only a loaded one.
    ///
    /// The guard reads "is the key on disk still the one boot minted". That was a set-once flag
    /// that no import cleared, so within one daemon lifetime: boot mints B, import X is allowed
    /// (B was boot-minted — right), and then import Z with `replace: false` ALSO succeeded,
    /// discarding X — the user's restored identity — with `replaced: false` and no refusal.
    ///
    /// Fixture discriminates: B, X and Z are three distinct keys, and X is read back from disk.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_import_without_replace_is_refused_and_keeps_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        // Boot: mint B, and record that it was minted this lifetime.
        let (b, created) = mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        assert!(created, "fixture: boot MINTED the key");
        mesh.set_user_key_path(key_path.clone());
        mesh.note_user_key_minted_at_boot(true);
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (x, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let (z, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("z.key")).unwrap();
        let x_uid = mcpmesh_trust::binding::user_id(&x);
        assert_ne!(
            mcpmesh_trust::binding::user_id(&b),
            x_uid,
            "fixture: B != X"
        );
        assert_ne!(
            mcpmesh_trust::binding::user_id(&z),
            x_uid,
            "fixture: Z != X"
        );
        let phrase = |k: &mcpmesh_trust::UserKey| {
            crate::pairing::recovery::encode(&k.signing_key().to_bytes())
        };

        // CONTROL: the first import over a boot-minted key needs no replace (the new-laptop path).
        let out = user_key_import(&state, phrase(&x), false)
            .await
            .expect("importing over a key boot minted seconds ago needs no replace");
        assert_eq!(out.user_id, x_uid);
        assert!(
            !out.replaced,
            "a boot-minted key is not a real identity discarded"
        );

        // THE assertion: X is now a real identity, and replace=false must defend it.
        let e = user_key_import(&state, phrase(&z), false)
            .await
            .expect_err("a second import without replace must not discard the first imported key");
        assert!(
            format!("{e:#}").contains("Pass replace to do it anyway"),
            "the refusal must be the replace guard's: {e:#}"
        );
        assert_eq!(
            std::fs::read(&key_path).unwrap(),
            x.signing_key().to_bytes().to_vec(),
            "X must still be the key on disk"
        );
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            x_uid,
            "and still presented"
        );

        // With replace, it goes through and says it discarded something.
        let out = user_key_import(&state, phrase(&z), true).await.unwrap();
        assert!(
            out.replaced,
            "replacing X discarded a real identity, and must say so"
        );
    }

    /// #221: the same guard across an enrollment. Import X, adopt Y (which does not touch the
    /// key), import Z with `replace: false` — X is the key on disk and must be defended.
    #[tokio::test(flavor = "multi_thread")]
    async fn import_adopt_import_without_replace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        mesh.set_user_key_path(key_path.clone());
        mesh.note_user_key_minted_at_boot(true);
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let (x, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let (z, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("z.key")).unwrap();
        let phrase = |k: &mcpmesh_trust::UserKey| {
            crate::pairing::recovery::encode(&k.signing_key().to_bytes())
        };

        user_key_import(&state, phrase(&x), false).await.unwrap();
        let y = crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:Y-someone-elses".into(),
            sig: "b64u:sig".into(),
        };
        adopt_hook(&mesh)(y.clone()).await.unwrap();

        user_key_import(&state, phrase(&z), false)
            .await
            .expect_err("X is on disk under the enrollment; replace=false must defend it");
        assert_eq!(
            std::fs::read(&key_path).unwrap(),
            x.signing_key().to_bytes().to_vec(),
            "X must still be the key on disk"
        );
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            y.user_pk,
            "and a refused import changes nothing live: still enrolled as Y"
        );
        assert!(
            mesh.adopted_binding_path().exists(),
            "the enrollment file is untouched"
        );
    }

    /// #221: a probe fired INSIDE a signing path, after its #86 checks and before it reads the
    /// key, recording whether `user_key_lock` is held at that moment. Deterministic: no race, no
    /// timing — the lock is either held there or it is not.
    struct LockProbe {
        fired: Arc<std::sync::atomic::AtomicBool>,
        held: Arc<std::sync::atomic::AtomicBool>,
    }

    impl LockProbe {
        fn install(mesh: &Arc<MeshState>) -> Self {
            let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let hook: crate::daemon::IdentityGateHook = Box::new({
                let (fired, held) = (fired.clone(), held.clone());
                // The hook is taken when it fires, so this clone does not outlive the call.
                let mesh = mesh.clone();
                move || {
                    Box::pin(async move {
                        held.store(
                            mesh.user_key_lock.try_lock().is_err(),
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        fired.store(true, std::sync::atomic::Ordering::SeqCst);
                    })
                }
            });
            *mesh.identity_gate_hook.lock().unwrap() = Some(hook);
            Self { fired, held }
        }

        fn assert_held(&self, what: &str) {
            assert!(
                self.fired.load(std::sync::atomic::Ordering::SeqCst),
                "fixture: {what} never reached the point between its checks and the key read"
            );
            assert!(
                self.held.load(std::sync::atomic::Ordering::SeqCst),
                "{what}: user_key_lock is NOT held between the #86 check and the key read, so an \
                 adoption or import can land there"
            );
        }
    }

    /// A key-holding mesh presenting its own key's binding, as boot leaves it.
    async fn key_holding_mesh(
        dir: &tempfile::TempDir,
    ) -> (
        Arc<MeshState>,
        mcpmesh_trust::UserKey,
        crate::control::DaemonState,
    ) {
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        let (own, _) = mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        mesh.set_user_key_path(key_path);
        let (user_pk, sig) = mcpmesh_trust::binding::present(&own, mesh.endpoint.id().as_bytes());
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk,
            sig,
        }));
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        (mesh, own, state)
    }

    fn someone_elses_binding() -> crate::pairing::rendezvous::SelfBinding {
        crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:someone-elses-identity".into(),
            sig: "b64u:their-sig".into(),
        }
    }

    /// The #86 gate's coded (-32602) refusal, and not some other failure.
    fn is_gate_refusal<T>(r: Result<T>) -> bool {
        matches!(r, Err(e) if e.downcast_ref::<crate::control::InvalidParams>().is_some()
            && format!("{e:#}").contains("does not hold that user key"))
    }

    /// #221: `peer_endorse` holds `user_key_lock` from its #86 check through the signature.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_endorse_holds_the_key_lock_from_its_gate_through_the_signature() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, own, state) = key_holding_mesh(&dir).await;
        let subject = iroh::SecretKey::from_bytes(&[0x91; 32]).public();
        let probe = LockProbe::install(&mesh);
        let out = endorse_peer(
            &state,
            mcpmesh_local_api::PeerEndorseParams {
                subject: subject.to_string(),
                subject_user_id: None,
            },
        )
        .await
        .expect("a key-holding device endorses");
        probe.assert_held("peer_endorse");
        assert_eq!(out.endorsed_by, mcpmesh_trust::binding::user_id(&own));
    }

    /// #221: the same for `device_revoke` — the stolen-laptop path.
    #[tokio::test(flavor = "multi_thread")]
    async fn device_revoke_holds_the_key_lock_from_its_gate_through_the_signature() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, own, state) = key_holding_mesh(&dir).await;
        let lost = iroh::SecretKey::from_bytes(&[0x77; 32]).public();
        let probe = LockProbe::install(&mesh);
        let out = device_revoke(
            &state,
            mcpmesh_local_api::DeviceRevokeParams {
                endpoint: mcpmesh_net::EndpointId::from_bytes(*lost.as_bytes()).principal(),
                reason: None,
            },
        )
        .await
        .expect("a key-holding device signs a revocation");
        probe.assert_held("device_revoke");
        assert_eq!(out.user_id, mcpmesh_trust::binding::user_id(&own));
    }

    /// #221 (from #219): the same for `user_key_export`, whose check used to run BEFORE the lock.
    #[tokio::test(flavor = "multi_thread")]
    async fn user_key_export_holds_the_key_lock_from_its_gate_through_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, own, state) = key_holding_mesh(&dir).await;
        let probe = LockProbe::install(&mesh);
        let out = user_key_export(&state)
            .await
            .expect("a key-holding device exports");
        probe.assert_held("user_key_export");
        assert_eq!(out.user_id, mcpmesh_trust::binding::user_id(&own));
    }

    /// #221: the same for `sign_binding`, driven through the `InviterCtx` closure the ceremony
    /// calls — and what it signs verifies under the `user_pk` the ceremony presents.
    #[tokio::test(flavor = "multi_thread")]
    async fn sign_binding_holds_the_key_lock_from_its_checks_through_the_signature() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, _state) = key_holding_mesh(&dir).await;
        let endpoint = [7u8; 32];
        let ctx = mesh.inviter_ctx();
        let presented = ctx.self_binding.clone().expect("presents its own");
        let probe = LockProbe::install(&mesh);
        let sig = (ctx.sign_binding)(endpoint)
            .await
            .expect("a key-holding device signs");
        probe.assert_held("sign_binding");
        mcpmesh_trust::binding::verify_presented(&presented.user_pk, &sig, &endpoint)
            .expect("the signature must verify under the identity the ceremony presents");
    }

    /// #221: the check must come AFTER the lock is taken, not before it. An adoption holding
    /// `user_key_lock` when a signing path starts (`adopt_hook` mid-write) must be SEEN.
    ///
    /// Deterministic on a current-thread runtime: the test holds the lock, spawns the path, and
    /// yields once — the path runs until it parks on the lock (a path that reads the gate first
    /// has read "not enrolled" by then). The test then installs the adoption as `adopt_hook` does
    /// under that lock and releases it.
    async fn outcome_when_an_adoption_holds_the_lock_at_start<T: Send + 'static>(
        mesh: &Arc<MeshState>,
        run: impl std::future::Future<Output = T> + Send + 'static,
    ) -> T {
        let guard = mesh.user_key_lock.lock().await;
        let path = tokio::spawn(run);
        tokio::task::yield_now().await;
        assert!(
            !path.is_finished(),
            "the signing path must be parked on user_key_lock"
        );
        mesh.set_self_binding_live(Some(someone_elses_binding()));
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(10), path)
            .await
            .expect("the signing path finishes once the lock is released")
            .expect("the signing path panicked")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_endorse_checks_its_gate_after_taking_the_key_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, state) = key_holding_mesh(&dir).await;
        let subject = iroh::SecretKey::from_bytes(&[0x91; 32]).public();
        let refused = outcome_when_an_adoption_holds_the_lock_at_start(&mesh, async move {
            is_gate_refusal(
                endorse_peer(
                    &state,
                    mcpmesh_local_api::PeerEndorseParams {
                        subject: subject.to_string(),
                        subject_user_id: None,
                    },
                )
                .await,
            )
        })
        .await;
        assert!(
            refused,
            "peer_endorse read the #86 gate before taking user_key_lock"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn device_revoke_checks_its_gate_after_taking_the_key_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, state) = key_holding_mesh(&dir).await;
        let lost = iroh::SecretKey::from_bytes(&[0x77; 32]).public();
        let refused = outcome_when_an_adoption_holds_the_lock_at_start(&mesh, async move {
            is_gate_refusal(
                device_revoke(
                    &state,
                    mcpmesh_local_api::DeviceRevokeParams {
                        endpoint: mcpmesh_net::EndpointId::from_bytes(*lost.as_bytes()).principal(),
                        reason: None,
                    },
                )
                .await,
            )
        })
        .await;
        assert!(
            refused,
            "device_revoke read the #86 gate before taking user_key_lock"
        );
        assert!(
            !mesh.store.is_revoked(lost.as_bytes()),
            "and nothing was self-applied"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn user_key_export_checks_its_gate_after_taking_the_key_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, state) = key_holding_mesh(&dir).await;
        let refused = outcome_when_an_adoption_holds_the_lock_at_start(&mesh, async move {
            is_gate_refusal(user_key_export(&state).await)
        })
        .await;
        assert!(
            refused,
            "user_key_export read the #86 gate before taking user_key_lock"
        );
    }

    /// The GATE refusal specifically: a `sign_binding` that read the gate before the lock would
    /// still refuse here, but as `IdentityChanged` — this pins which check saw the adoption.
    #[tokio::test(flavor = "current_thread")]
    async fn sign_binding_checks_its_gate_after_taking_the_key_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, _state) = key_holding_mesh(&dir).await;
        let presented = mesh.self_binding().expect("presents its own");
        let outcome = outcome_when_an_adoption_holds_the_lock_at_start(&mesh, {
            let mesh = mesh.clone();
            async move {
                mesh.sign_ceremony_binding(Some(&presented), &[7u8; 32])
                    .await
            }
        })
        .await;
        assert_eq!(
            outcome,
            Err(crate::daemon::SignBindingRefusal::Enrolled),
            "sign_binding must see the adoption at its #86 gate, under the lock"
        );
    }

    /// #221: `sign_binding` re-checks, under `user_key_lock`, that this node still presents the
    /// binding `inviter_ctx` snapshotted for the ceremony. An IMPORT between the snapshot and the
    /// signature would otherwise sign under X while the reply carries B's `user_pk` — a binding
    /// that does not verify under the identity the redeemer was told it joined.
    #[tokio::test(flavor = "multi_thread")]
    async fn sign_binding_refuses_after_an_import_since_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, state) = key_holding_mesh(&dir).await;
        let endpoint = [7u8; 32];
        let ctx = mesh.inviter_ctx();
        assert!(
            (mesh.inviter_ctx().sign_binding)(endpoint).await.is_some(),
            "control: with no change since the snapshot, a key-holding device signs"
        );

        let (x, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let phrase = crate::pairing::recovery::encode(&x.signing_key().to_bytes());
        user_key_import(&state, phrase, true).await.unwrap();

        assert!(
            (ctx.sign_binding)(endpoint).await.is_none(),
            "an import since the snapshot: the key on disk is no longer the presented identity's"
        );
        assert_eq!(
            mesh.sign_ceremony_binding(ctx.self_binding.as_ref(), &endpoint)
                .await,
            Err(crate::daemon::SignBindingRefusal::IdentityChanged),
            "refused by the snapshot comparison, not only by the key check behind it"
        );
        assert!(
            (mesh.inviter_ctx().sign_binding)(endpoint).await.is_some(),
            "a ceremony accepted AFTER the import presents X and signs under X"
        );
    }

    /// #221: the same for an ADOPTION between the snapshot and the signature — the ceremony
    /// presented this device's own identity, and the device no longer holds authority over what it
    /// presents.
    #[tokio::test(flavor = "multi_thread")]
    async fn sign_binding_refuses_after_an_adoption_since_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, _state) = key_holding_mesh(&dir).await;
        let endpoint = [7u8; 32];
        let ctx = mesh.inviter_ctx();
        adopt_hook(&mesh)(someone_elses_binding()).await.unwrap();
        assert!(
            (ctx.sign_binding)(endpoint).await.is_none(),
            "an adoption since the snapshot must refuse the signature"
        );
    }

    /// #221 review (probe): `user.key` deleted under a running node. `peer_endorse` used
    /// `load_or_generate` and MINTED key C in its place; the next self-enrollment ceremony compared
    /// snapshots (B == B), read C and signed under C while presenting B — and the redeemer refused
    /// that binding after the single-use invite was already burned. Now the endorse refuses and
    /// mints nothing, and the ceremony refuses too.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_user_key_is_not_minted_by_endorse_and_the_ceremony_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, state) = key_holding_mesh(&dir).await;
        let key_path = mesh.user_key_path.get().cloned().unwrap();
        let subject = iroh::SecretKey::from_bytes(&[0x91; 32]).public();
        let endorse = || {
            endorse_peer(
                &state,
                mcpmesh_local_api::PeerEndorseParams {
                    subject: subject.to_string(),
                    subject_user_id: None,
                },
            )
        };
        let ctx = mesh.inviter_ctx();
        // Control: with the key in place, both work.
        endorse()
            .await
            .expect("control: endorse with the key present");
        assert!(
            (ctx.sign_binding)([7u8; 32]).await.is_some(),
            "control: the ceremony signs with the key present"
        );

        std::fs::remove_file(&key_path).unwrap();
        let e = endorse()
            .await
            .expect_err("no key on disk: endorse must refuse, not mint");
        assert!(
            format!("{e:#}").contains("has no user key"),
            "and say why: {e:#}"
        );
        assert!(
            !key_path.exists(),
            "endorse must not MINT a replacement key underneath the presented identity"
        );
        assert!(
            (ctx.sign_binding)([7u8; 32]).await.is_none(),
            "the ceremony presents B and must not sign with a key that is not B"
        );
    }

    /// #221 review: the key-on-disk check itself. `user.key` replaced underneath the running node
    /// with C (by any means) while it presents B: snapshots agree, so only comparing the key read to
    /// the presented `user_pk` refuses.
    #[tokio::test(flavor = "multi_thread")]
    async fn sign_binding_refuses_a_key_on_disk_that_is_not_the_presented_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _own, _state) = key_holding_mesh(&dir).await;
        let key_path = mesh.user_key_path.get().cloned().unwrap();
        let ctx = mesh.inviter_ctx();
        let presented = ctx.self_binding.clone().unwrap();
        let sig = (ctx.sign_binding)([7u8; 32])
            .await
            .expect("control: B on disk signs");
        mcpmesh_trust::binding::verify_presented(&presented.user_pk, &sig, &[7u8; 32])
            .expect("control: and verifies under B");

        let (c, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("c.key")).unwrap();
        std::fs::copy(dir.path().join("c.key"), &key_path).unwrap();
        assert_eq!(
            std::fs::read(&key_path).unwrap(),
            c.signing_key().to_bytes().to_vec(),
            "fixture: C is on disk"
        );
        assert_eq!(
            mesh.self_binding().as_ref(),
            Some(&presented),
            "fixture: the snapshot comparison cannot see this"
        );
        assert!(
            (ctx.sign_binding)([7u8; 32]).await.is_none(),
            "C on disk while presenting B: a signature under C would not verify under B"
        );
        assert_eq!(
            mesh.sign_ceremony_binding(Some(&presented), &[7u8; 32])
                .await,
            Err(crate::daemon::SignBindingRefusal::KeyMismatch)
        );
    }

    /// #86 gate: the adopt hook must actually WRITE the file. It is "the only copy there will ever
    /// be" — and no test connected the write path to the read path, so deleting the persist step
    /// passed the whole workspace. The boot test writes the file itself, which proves the reader.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_adopt_hook_persists_the_binding_and_installs_it_live() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        mesh.set_user_key_path(dir.path().join("user.key"));

        let binding = crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:someone-elses-identity".into(),
            sig: "b64u:sig".into(),
        };
        adopt_hook(&mesh)(binding.clone())
            .await
            .expect("the hook persists and installs");

        // On DISK — an enrolled device holds no user key and cannot re-derive this.
        let path = mesh.adopted_binding_path();
        let from_disk: crate::pairing::rendezvous::SelfBinding =
            serde_json::from_slice(&std::fs::read(&path).expect("the binding was written"))
                .expect("and is readable");
        assert_eq!(from_disk, binding, "the file must round-trip the binding");

        // …and LIVE, so the device presents it without a restart.
        assert_eq!(
            mesh.self_binding().expect("a binding").user_pk,
            binding.user_pk,
            "the adopted binding must take effect immediately, not only after a restart"
        );
    }

    /// #214 fixture: a mesh that holds its OWN key (boot-derived binding + key file) and was then
    /// ENROLLED into someone else's identity through the real `adopt_hook`, so the sidecar exists
    /// on disk and the live slot is set — the state every #214 verb exists to act on.
    async fn enrolled_mesh(
        dir: &tempfile::TempDir,
    ) -> (
        Arc<MeshState>,
        crate::audit::AuditSink,
        crate::pairing::rendezvous::SelfBinding,
    ) {
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        let (own, _) = mcpmesh_trust::UserKey::load_or_generate(&key_path).unwrap();
        mesh.set_user_key_path(key_path);
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: mcpmesh_trust::binding::user_id(&own),
            sig: "b64u:own-sig".into(),
        }));
        let audit =
            crate::audit::AuditSink::new(crate::audit::AuditLog::spawn(dir.path().join("audit")));
        mesh.set_audit(audit.clone());
        let adopted = crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:someone-elses-identity".into(),
            sig: "b64u:their-sig".into(),
        };
        adopt_hook(&mesh)(adopted.clone())
            .await
            .expect("the hook persists and installs");
        assert!(
            mesh.adopted_binding_path().exists(),
            "precondition: the enrollment is on disk"
        );
        (mesh, audit, adopted)
    }

    /// Drain the live audit ring into a Vec (the ring, not the file, so nothing races the flush).
    fn drain(rx: &mut tokio::sync::broadcast::Receiver<AuditRecord>) -> Vec<AuditRecord> {
        let mut recs = Vec::new();
        while let Ok(r) = rx.try_recv() {
            recs.push(r);
        }
        recs
    }

    /// #214 ask 1: the exit. Detaching must undo BOTH halves of `adopt_hook` — the sidecar boot
    /// re-reads and the live slot — audit it, and report the identity now in effect.
    #[tokio::test(flavor = "multi_thread")]
    async fn self_enroll_detach_drops_the_binding_removes_the_sidecar_and_audits() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, audit, adopted) = enrolled_mesh(&dir).await;
        let own_uid = mesh.self_binding.get().cloned().flatten().unwrap().user_pk;
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            adopted.user_pk,
            "precondition: the enrolled device presents the ADOPTED identity"
        );
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let mut rx = audit.subscribe().expect("auditing enabled");

        let out = self_enroll_detach(&state)
            .await
            .expect("an enrolled device can detach");

        assert_eq!(
            out.detached_from, adopted.user_pk,
            "names the identity left"
        );
        assert_eq!(
            out.user_id.as_deref(),
            Some(own_uid.as_str()),
            "and the identity now in effect — this device's own"
        );
        assert!(
            mesh.adopted_binding.read().expect("lock").is_none(),
            "the live slot must be cleared: `self_user_key_held` and the three gates read it"
        );
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            own_uid,
            "this device's own identity is presented again, without a restart"
        );
        assert!(
            !mesh.adopted_binding_path().exists(),
            "the sidecar must be GONE, or the next boot silently re-enrolls this device"
        );
        let rec = drain(&mut rx)
            .into_iter()
            .find(|r| r.event.as_deref() == Some("self_enroll_detach"))
            .expect("leaving an identity is a trust change an operator asks about later");
        assert_eq!(rec.target.as_deref(), Some(adopted.user_pk.as_str()));
        assert!(
            (mesh.inviter_ctx().sign_binding)([7u8; 32]).await.is_some(),
            "…and having left, this device can enroll its owner's other devices again"
        );
    }

    /// #214 ask 1: nothing adopted → a CODED refusal, and nothing touched. A live-only adoption
    /// (never persisted) still detaches — what matters is what boot would re-read.
    #[tokio::test(flavor = "multi_thread")]
    async fn self_enroll_detach_refuses_when_nothing_is_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        mesh.set_user_key_path(dir.path().join("user.key"));
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:own".into(),
            sig: "b64u:sig".into(),
        }));
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let e = self_enroll_detach(&state)
            .await
            .expect_err("a device holding its own key has nothing to detach");
        let code = e
            .downcast_ref::<crate::pairing::rendezvous::PairRefusal>()
            .map(|r| r.code());
        assert_eq!(
            code,
            Some(mcpmesh_local_api::ERR_NOT_ENROLLED),
            "coded, so a UI can treat it as 'already done': {e:#}"
        );
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            "b64u:own",
            "the refusal must not disturb the identity presented"
        );

        // Live-only adoption: the slot is set, no sidecar was ever written.
        mesh.set_self_binding_live(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:theirs".into(),
            sig: "b64u:sig".into(),
        }));
        let out = self_enroll_detach(&state)
            .await
            .expect("a missing sidecar is not a failure to detach");
        assert_eq!(out.detached_from, "b64u:theirs");
        assert!(mesh.adopted_binding.read().expect("lock").is_none());
    }

    /// #214: the three writers of the adopted slot + sidecar — adopt, detach, import — serialize
    /// on `user_key_lock`. A detach racing an adoption could otherwise remove the sidecar, lose to
    /// the adopt's write, then clear the live slot: a file the next boot re-adopts under a `status`
    /// that says the exit took. Pinned by holding the lock and asserting neither completes until
    /// it is released (bounded waits, never a spin).
    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_and_detach_serialize_on_the_user_key_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _audit, _adopted) = enrolled_mesh(&dir).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let binding = crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:third".into(),
            sig: "b64u:sig".into(),
        };

        let held = mesh.user_key_lock.lock().await;
        let blocked = tokio::time::timeout(
            Duration::from_millis(300),
            adopt_hook(&mesh)(binding.clone()),
        )
        .await;
        assert!(
            blocked.is_err(),
            "adopt must wait for the user key lock: {blocked:?}"
        );
        let blocked =
            tokio::time::timeout(Duration::from_millis(300), self_enroll_detach(&state)).await;
        assert!(
            blocked.is_err(),
            "detach must wait for the user key lock: {blocked:?}"
        );
        drop(held);

        // Released: both run, in order, to a consistent end state.
        adopt_hook(&mesh)(binding.clone())
            .await
            .expect("adopt proceeds once the lock is free");
        let out = tokio::time::timeout(Duration::from_secs(10), self_enroll_detach(&state))
            .await
            .expect("detach proceeds once the lock is free")
            .expect("and succeeds");
        assert_eq!(out.detached_from, "b64u:third");
        assert!(!mesh.adopted_binding_path().exists());
        assert!(mesh.adopted_binding.read().expect("lock").is_none());
    }

    /// #214 review: a sidecar with NO live adoption is reachable (boot leaves a file that does not
    /// verify; a pre-#214 import removed it only best-effort), and the next boot re-reads it. Detach must remove it even while answering "nothing adopted", or the answer is
    /// undone at the next restart.
    #[tokio::test(flavor = "multi_thread")]
    async fn self_enroll_detach_removes_a_stale_sidecar_even_when_nothing_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        mesh.set_user_key_path(dir.path().join("user.key"));
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:own".into(),
            sig: "b64u:sig".into(),
        }));
        let stale = mesh.adopted_binding_path();
        std::fs::write(
            &stale,
            serde_json::to_vec(&crate::pairing::rendezvous::SelfBinding {
                user_pk: "b64u:left-behind".into(),
                sig: "b64u:sig".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(mesh.adopted_binding.read().expect("lock").is_none());
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let e = self_enroll_detach(&state)
            .await
            .expect_err("nothing is adopted LIVE, so the answer is still the coded refusal");
        assert_eq!(
            e.downcast_ref::<crate::pairing::rendezvous::PairRefusal>()
                .map(|r| r.code()),
            Some(mcpmesh_local_api::ERR_NOT_ENROLLED),
            "{e:#}"
        );
        assert!(
            !stale.exists(),
            "the stale sidecar must be GONE — boot would have re-adopted it"
        );
        assert!(
            format!("{e:#}").contains("stale enrollment file"),
            "and the refusal must say a file was removed, since 'already done' was not true on \
             disk: {e:#}"
        );

        // With nothing on disk either, the plain refusal.
        let e = self_enroll_detach(&state)
            .await
            .expect_err("still nothing adopted");
        assert!(
            !format!("{e:#}").contains("stale"),
            "no file, no claim that one was removed: {e:#}"
        );
    }

    /// #214 review: an import on an ENROLLED device must fail CLOSED when the enrollment file
    /// cannot be removed. Warn-and-continue installed X live while the next boot re-adopted Y, with
    /// `self_user_key_held: true` and nothing in `status` to reveal it. Removal is made to fail by
    /// putting a DIRECTORY at the sidecar path — the key file beside it stays writable, so the
    /// old order (key first, then a warned removal) would SUCCEED here and this test would see it.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_that_cannot_remove_the_enrollment_file_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _audit, adopted) = enrolled_mesh(&dir).await;
        mesh.note_user_key_minted_at_boot(true);
        let sidecar = mesh.adopted_binding_path();
        std::fs::remove_file(&sidecar).unwrap();
        std::fs::create_dir(&sidecar).unwrap();
        std::fs::write(sidecar.join("pin"), b"x").unwrap();
        let key_path = mesh.user_key_path.get().cloned().unwrap();
        let key_before = std::fs::read(&key_path).unwrap();
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (x, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let phrase = crate::pairing::recovery::encode(&x.signing_key().to_bytes());
        let e = user_key_import(&state, phrase, true)
            .await
            .expect_err("an import the next boot would silently undo must be refused");
        assert!(
            format!("{e:#}").contains(&sidecar.display().to_string()),
            "the error must name the file: {e:#}"
        );
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            adopted.user_pk,
            "nothing changes live: still the enrolled identity"
        );
        assert!(
            !crate::control::status_result(&state)
                .unwrap()
                .self_user_key_held
        );
        assert!(sidecar.exists(), "the enrollment file is untouched");
        assert_eq!(
            std::fs::read(&key_path).unwrap(),
            key_before,
            "and so is the key on disk — the live slot and user.key still agree with boot"
        );
    }

    /// #214 review: the REMOVE branch of the fail-closed import, as distinct from the read branch
    /// above (a directory fails the read before the remove is reached). A read-only state directory
    /// lets the file be read but not unlinked. The key write would fail there too, so the
    /// discriminating assertion is WHICH file the error names: removing first names the sidecar,
    /// and a warned removal would have fallen through to a key-write error instead.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_that_cannot_unlink_the_enrollment_file_names_it_and_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _audit, adopted) = enrolled_mesh(&dir).await;
        mesh.note_user_key_minted_at_boot(true);
        let (x, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let phrase = crate::pairing::recovery::encode(&x.signing_key().to_bytes());
        let sidecar = mesh.adopted_binding_path();
        let state_dir = sidecar.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores directory permissions; the precondition makes that a skip, not a pass.
        let probe = state_dir.join("probe");
        if std::fs::write(&probe, b"x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipping: directory permissions are not enforced for this user");
            return;
        }
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let e = user_key_import(&state, phrase, true).await;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = e.expect_err("the enrollment file cannot be removed");
        assert!(
            format!("{e:#}").contains("remove the enrollment binding"),
            "the error must be the SIDECAR removal, reached before any key write: {e:#}"
        );
        assert_eq!(mesh.self_binding().unwrap().user_pk, adopted.user_pk);
        assert!(sidecar.exists());
    }

    /// #214 review: the other failure branch — the sidecar was removed, then the KEY write fails.
    /// The sidecar must be written back, so the node is exactly as it was. The key write is made to
    /// fail with a directory at `user.key` (a rename over a directory fails); the sidecar's own
    /// directory stays writable, so the restore succeeds.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_whose_key_write_fails_restores_the_enrollment_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        std::fs::create_dir(&key_path).unwrap();
        std::fs::write(key_path.join("pin"), b"x").unwrap();
        mesh.set_user_key_path(key_path.clone());
        mesh.note_user_key_minted_at_boot(true);
        let adopted = crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:Y".into(),
            sig: "b64u:sig".into(),
        };
        adopt_hook(&mesh)(adopted.clone()).await.unwrap();
        let sidecar = mesh.adopted_binding_path();
        let sidecar_before = std::fs::read(&sidecar).unwrap();
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (x, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let phrase = crate::pairing::recovery::encode(&x.signing_key().to_bytes());
        user_key_import(&state, phrase, true)
            .await
            .expect_err("the key cannot be written");
        assert_eq!(
            std::fs::read(&sidecar).ok(),
            Some(sidecar_before),
            "the enrollment file must be RESTORED — otherwise the next boot presents this device's \
             own identity while it presents Y now"
        );
        assert_eq!(mesh.self_binding().unwrap().user_pk, adopted.user_pk);
    }

    /// #214 review: import X, then adopt Y. The enrollment must WIN (`self_binding()` is Y,
    /// `status` reports Y and `self_user_key_held: false`), and a detach must fall back to X — the
    /// identity whose key is actually on disk — not the pre-import boot binding. The first version
    /// let the imported slot outrank the adopted one, so the node presented X live while the
    /// sidecar held Y and `self_user_key_held` contradicted `self_user_id`.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_enrollment_supersedes_an_imported_key_and_a_detach_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let key_path = dir.path().join("user.key");
        mesh.set_user_key_path(key_path.clone());
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:boot".into(),
            sig: "b64u:sig".into(),
        }));
        mesh.note_user_key_minted_at_boot(true);
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // Import X.
        let (x, _) = mcpmesh_trust::UserKey::load_or_generate(&dir.path().join("x.key")).unwrap();
        let x_uid = mcpmesh_trust::binding::user_id(&x);
        let phrase = crate::pairing::recovery::encode(&x.signing_key().to_bytes());
        let out = user_key_import(&state, phrase, false).await.unwrap();
        assert_eq!(out.user_id, x_uid);
        assert_eq!(mesh.self_binding().unwrap().user_pk, x_uid, "precondition");

        // Adopt Y.
        let y = crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:Y-someone-elses".into(),
            sig: "b64u:sig".into(),
        };
        adopt_hook(&mesh)(y.clone()).await.unwrap();
        assert_eq!(
            mesh.self_binding().unwrap().user_pk,
            y.user_pk,
            "the enrollment is the most recent act and must be what this device presents"
        );
        let status = crate::control::status_result(&state).unwrap();
        assert_eq!(status.self_user_id.as_deref(), Some(y.user_pk.as_str()));
        assert!(
            !status.self_user_key_held,
            "enrolled: the key behind Y lives elsewhere, whatever is on disk"
        );

        // Detach → X, not the boot binding: X's key is the one on disk.
        let out = self_enroll_detach(&state).await.unwrap();
        assert_eq!(out.detached_from, y.user_pk);
        assert_eq!(
            out.user_id.as_deref(),
            Some(x_uid.as_str()),
            "the identity restored must be the IMPORTED one — its key is what `user.key` holds"
        );
        assert_eq!(mesh.self_binding().unwrap().user_pk, x_uid);
        let status = crate::control::status_result(&state).unwrap();
        assert_eq!(status.self_user_id.as_deref(), Some(x_uid.as_str()));
        assert!(status.self_user_key_held);
    }

    /// #214 ask 3: `status.self_user_key_held` is the one field that tells an enrolled device from
    /// one holding its key — both report the same `self_user_id` shape. Pinned through every
    /// transition, because a constant `true` passes any single-state assertion.
    #[tokio::test(flavor = "multi_thread")]
    async fn status_reports_whether_this_device_holds_its_user_key() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        mesh.set_user_key_path(dir.path().join("user.key"));
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // No user key at all: nothing is held.
        let status = crate::control::status_result(&state).unwrap();
        assert!(status.self_user_id.is_none());
        assert!(
            !status.self_user_key_held,
            "with no identity there is no key to hold"
        );

        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:own".into(),
            sig: "b64u:sig".into(),
        }));
        let status = crate::control::status_result(&state).unwrap();
        assert_eq!(status.self_user_id.as_deref(), Some("b64u:own"));
        assert!(
            status.self_user_key_held,
            "a device presenting its boot-derived binding holds the key behind it"
        );

        mesh.set_self_binding_live(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:theirs".into(),
            sig: "b64u:sig".into(),
        }));
        let status = crate::control::status_result(&state).unwrap();
        assert_eq!(
            status.self_user_id.as_deref(),
            Some("b64u:theirs"),
            "an enrolled device reports the ADOPTED id — the same shape as a key-holder"
        );
        assert!(
            !status.self_user_key_held,
            "…which is exactly why this field exists: the key lives on the enrolling device"
        );

        self_enroll_detach(&state).await.unwrap();
        let status = crate::control::status_result(&state).unwrap();
        assert_eq!(status.self_user_id.as_deref(), Some("b64u:own"));
        assert!(status.self_user_key_held, "held again once detached");
    }

    /// #214 ask 4 (BUG): `device_revoke` on an ENROLLED device signed with the LOCAL key, applied
    /// the revocation here, severed sessions, audited it and returned a token no peer would ever
    /// accept — a silent partial success on the stolen-laptop path. It must refuse with
    /// `peer_endorse`'s #86 gate, BEFORE any of those side effects.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_enrolled_device_cannot_sign_a_device_revocation_and_nothing_is_applied() {
        use mcpmesh_local_api::DeviceRevokeParams;

        let dir = tempfile::tempdir().unwrap();
        let (mesh, audit, _adopted) = enrolled_mesh(&dir).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let lost = iroh::SecretKey::from_bytes(&[0x77; 32]).public();
        let lost_bytes = *lost.as_bytes();
        // The stolen laptop is still paired here — the realistic shape, and what makes a
        // self-applied revocation observable.
        mesh.store
            .add(PeerEntry {
                endpoint_id: lost_bytes,
                nickname: "old-laptop".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();
        let severed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        mesh.set_sever_observer({
            let severed = severed.clone();
            move |_| severed.store(true, std::sync::atomic::Ordering::SeqCst)
        });
        let mut rx = audit.subscribe().expect("auditing enabled");
        let params = || DeviceRevokeParams {
            endpoint: mcpmesh_net::EndpointId::from_bytes(lost_bytes).principal(),
            reason: Some("stolen".into()),
        };

        let e = device_revoke(&state, params())
            .await
            .expect_err("an enrolled device must refuse to sign a device revocation");
        assert!(
            e.downcast_ref::<crate::control::InvalidParams>().is_some(),
            "the same shape as peer_endorse's #86 gate (-32602): {e:#}"
        );
        assert!(
            format!("{e:#}").contains("does not hold that user key"),
            "and the same message shape, saying WHY: {e:#}"
        );
        // None of the three side effects happened.
        assert!(
            !mesh.store.is_revoked(&lost_bytes),
            "the revocation must not be self-applied: peers would never act on the token, so this \
             node acting on it alone is the silent partial success"
        );
        assert!(
            !severed.load(std::sync::atomic::Ordering::SeqCst),
            "no session may be severed by a refused revocation"
        );
        assert!(
            drain(&mut rx)
                .iter()
                .all(|r| r.event.as_deref() != Some("device_revoke")),
            "a refused revocation must not be audited as one"
        );

        // The gate is the ADOPTED slot, nothing else: detach, and the same call signs.
        self_enroll_detach(&state).await.unwrap();
        let out = device_revoke(&state, params())
            .await
            .expect("a device holding its own key signs");
        assert_eq!(
            Some(out.user_id.as_str()),
            crate::control::status_result(&state)
                .unwrap()
                .self_user_id
                .as_deref(),
            "the token is signed under the identity this node PRESENTS — the one peers pinned"
        );
        assert!(mesh.store.is_revoked(&lost_bytes));
    }

    /// #214 ask 5: `invite { as_self }` on an ENROLLED device is refused AT MINT with its own
    /// code, on the machine that can act on it — not one round trip later on the other machine as
    /// the opaque -32049. The enrolled check must come BEFORE the no-identity one: an enrolled
    /// device's `self_binding()` is `Some` (the adopted one), so the generic message would win.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_enrolled_device_cannot_mint_a_self_invite() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _audit, _adopted) = enrolled_mesh(&dir).await;

        let e = mint_invite(vec![], None, None, None, true, &mesh)
            .await
            .expect_err("an enrolled device holds no key to enroll another with");
        let code = e
            .downcast_ref::<crate::pairing::rendezvous::PairRefusal>()
            .map(|r| r.code());
        assert_eq!(
            code,
            Some(mcpmesh_local_api::ERR_SELF_ENROLL_NO_KEY),
            "coded, so a UI renders a permanent condition rather than 'ask for a new one': {e:#}"
        );
        let msg = format!("{e:#}");
        assert!(
            msg.contains("enrolled into another device's identity"),
            "the SPECIFIC message, not the generic no-identity one: {msg}"
        );
        assert_eq!(
            mesh.invites.count(),
            0,
            "a refused mint must leave no outstanding invite"
        );

        // An ORDINARY invite still mints on an enrolled device — it grants access, not identity.
        std::fs::write(
            &mesh.config_path,
            "[services.notes]\nsocket = \"/run/n.sock\"\nallow = []\n",
        )
        .unwrap();
        mint_invite(vec!["notes".into()], None, None, None, false, &mesh)
            .await
            .expect("an ordinary invite is unaffected");

        // Detached, the same device mints a self invite again: the gate is the adopted slot.
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        self_enroll_detach(&state).await.unwrap();
        mint_invite(vec![], None, None, None, true, &mesh)
            .await
            .expect("a device holding its own key enrolls others");

        // With no identity at all the mint is refused too, uncoded — it was refused on the OTHER
        // machine before, which is the #159 shape.
        let dir2 = tempfile::tempdir().unwrap();
        let config_path = dir2.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let keyless = hermetic_mesh(config_path).await;
        let e = mint_invite(vec![], None, None, None, true, &keyless)
            .await
            .expect_err("no identity to enroll into");
        assert!(
            e.downcast_ref::<crate::pairing::rendezvous::PairRefusal>()
                .is_none()
                && format!("{e:#}").contains("no user identity to enroll into"),
            "{e:#}"
        );
    }

    /// #86: a self-enrollment invite mints IDENTITY, so both guards are refusals, not warnings.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_self_invite_must_be_single_use_and_grant_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.notes]\nsocket = \"/run/n.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        // #214: a self invite needs an identity to enroll INTO, or the mint itself is refused.
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:own".into(),
            sig: "b64u:sig".into(),
        }));
        let svc = || vec!["notes".to_string()];

        // Multi-use: a standing offer to become this person.
        let e = mint_invite(vec![], None, Some(3), None, true, &mesh)
            .await
            .expect_err("a multi-use SELF invite must be refused");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("as_self") && msg.contains('3'),
            "the error must name the flag and the max_uses: {msg}"
        );
        assert!(
            msg.contains("become this person"),
            "and say WHY, since the caller asked for something reasonable-looking: {msg}"
        );

        // Services: your own devices are not peers of each other.
        let e = mint_invite(svc(), None, None, None, true, &mesh)
            .await
            .expect_err("a SELF invite that grants services must be refused");
        assert!(format!("{e:#}").contains("grants nothing"), "{e:#}");

        // The legal shape mints, and the flag reaches the LINE — the redeemer reads it from there
        // to know it must adopt rather than pair.
        let minted = mint_invite(vec![], None, None, None, true, &mesh)
            .await
            .expect("a single-use, grant-free self invite is legal");
        let decoded = crate::pairing::Invite::decode(&minted.invite_line).unwrap();
        assert!(
            decoded.as_self,
            "as_self must ride the invite line, or the redeemer pairs instead of enrolling"
        );

        // And an ORDINARY invite is unaffected — the flag must not change the default path.
        let ordinary = mint_invite(svc(), None, None, None, false, &mesh)
            .await
            .expect("an ordinary invite still mints");
        assert!(
            !crate::pairing::Invite::decode(&ordinary.invite_line)
                .unwrap()
                .as_self
        );
    }

    /// #87: `peer_nickname` is the inviter's PRIVATE local name for the redeemer. Two properties,
    /// and the second is the one a reviewer should check hardest: it must never ride the invite
    /// LINE, which is a copyable artifact people paste into chats.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_nickname_is_stored_but_never_travels_on_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.notes]\nsocket = \"/run/notes.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let svc = || vec!["notes".to_string()];
        let minted = mint_invite(svc(), None, None, Some("their-laptop".into()), false, &mesh)
            .await
            .expect("an alias on a single-use invite is fine");

        // The LINE must not carry it — the redeemer has no business knowing what we call them.
        let decoded = crate::pairing::Invite::decode(&minted.invite_line).expect("line decodes");
        assert_eq!(
            decoded.peer_nickname, None,
            "the inviter's local alias must be STRIPPED from the invite line: {decoded:?}"
        );
        // Belt and braces: it must not appear anywhere in the encoded artifact, in any form.
        assert!(
            !minted.invite_line.contains("their-laptop"),
            "the alias must not be recoverable from the line at all"
        );

        // …but the daemon must still HOLD it, or the invite it applies to is the one that
        // survives a restart and the alias would be silently lost.
        let held = mesh
            .invites
            .peek_live_alias(&decoded.secret, crate::util::epoch_now_u64())
            .expect("the invite is live");
        assert_eq!(
            held.as_deref(),
            Some("their-laptop"),
            "the alias must be retained daemon-side — it is stripped from the line, so this is \
             the only place it can live"
        );
    }

    /// #87: one alias applied to every redeemer of a multi-use invite collides on the SECOND
    /// redemption. Refused at mint rather than producing an invite that works exactly once.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_nickname_is_refused_with_a_multi_use_invite_and_when_blank() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.notes]\nsocket = \"/run/notes.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let svc = || vec!["notes".to_string()];

        let e = mint_invite(svc(), None, Some(3), Some("them".into()), false, &mesh)
            .await
            .expect_err("an alias on a multi-use invite must be refused at MINT");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("peer_nickname") && msg.contains('3'),
            "the error must name the field and the max_uses it conflicts with: {msg}"
        );
        assert!(
            msg.contains("peer_rename"),
            "and point at the recovery, or the caller has to guess: {msg}"
        );

        // Blank is rejected rather than treated as absent — a UI passing through an untouched
        // field must not silently get the name it was trying to avoid.
        for blank in ["", "   "] {
            mint_invite(svc(), None, None, Some(blank.into()), false, &mesh)
                .await
                .expect_err("a blank alias must be a clean error, not a silent fallback");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn max_uses_is_clamped_and_zero_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.notes]\nsocket = \"/run/notes.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let svc = || vec!["notes".to_string()];

        let one = mint_invite(svc(), None, None, None, false, &mesh)
            .await
            .unwrap();
        assert_eq!(one.uses_remaining, 1, "absent means single-use");

        let three = mint_invite(svc(), None, Some(3), None, false, &mesh)
            .await
            .unwrap();
        assert_eq!(three.uses_remaining, 3);

        let capped = mint_invite(svc(), None, Some(10_000), None, false, &mesh)
            .await
            .unwrap();
        assert_eq!(
            capped.uses_remaining,
            mcpmesh_local_api::MAX_INVITE_USES,
            "over the cap is clamped, and the caller is told the value it ACTUALLY got"
        );

        let err = mint_invite(svc(), None, Some(0), None, false, &mesh)
            .await
            .expect_err("zero redemptions is a caller bug, not a valid invite");
        assert!(
            err.downcast_ref::<crate::control::InvalidParams>()
                .is_some(),
            "and it must be branchable as -32602 invalid params, not a generic failure: {err}"
        );
    }

    /// #87b gate: the DAEMON path really persists. `mint_invite` -> a file on disk.
    ///
    /// Every other test for this drove `LiveInvites`/`InviteFile` directly, which left the wiring
    /// unpinned: replacing `LiveInvites::load(paths.invites_path, ..)` with `LiveInvites::new()`
    /// in boot — reverting the ENTIRE feature — passed the whole workspace. A durable-invite
    /// feature that no test notices the absence of is not a feature.
    #[tokio::test(flavor = "multi_thread")]
    async fn minting_an_invite_writes_it_to_the_configured_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.notes]
socket = \"/run/notes.sock\"
allow = []
",
        )
        .unwrap();
        let invites_path = dir.path().join("invites.json");

        // A file-backed registry, exactly as `boot_node` builds one.
        let mesh = crate::daemon::testutil::hermetic_mesh_with_invites(
            config_path,
            Arc::new(crate::pairing::LiveInvites::load(
                invites_path.clone(),
                crate::util::epoch_now_u64(),
            )),
        )
        .await;

        let res = mint_invite(vec!["notes".into()], None, None, None, false, &mesh)
            .await
            .expect("mint");
        assert!(res.invite_line.starts_with("mcpmesh-invite:"));

        let on_disk = crate::pairing::persist::InviteFile::new(&invites_path).load(0);
        assert_eq!(
            on_disk.len(),
            1,
            "the `invite` verb must leave the invite ON DISK — otherwise the 24h TTL it just \
             advertised is a promise the next restart breaks (#87b)"
        );
        assert_eq!(
            on_disk[0].expires_at_epoch, res.expires_at_epoch,
            "and it must be THE invite that was handed out"
        );
    }

    /// #140 gate: the diagnostic must NOT dial. It reads the reachability cache.
    ///
    /// The first version called `reachability_of`, which spawns a background probe for every peer
    /// whose entry is stale or missing — so a verb documented as "probes nothing, dials nothing"
    /// dialed EVERY paired peer, wrote both caches, pushed `Reachability` frames at subscribers,
    /// and spent the peer's #89 ping budget. On a freshly restarted daemon that is one dial per
    /// peer, every time. A diagnostic used ON a live reproduction must observe it, not join it.
    ///
    /// `probe_seq` is the witness: `probe_peer` takes a ticket before anything else, so the
    /// counter moving is proof a dial started.
    /// **The gate deleted the entire `remote_info` call and the whole suite stayed green.**
    ///
    /// Nothing constructed a `KnownAddr`, exercised the `active` mapping, or labelled an
    /// iroh-supplied address — the fields were computed by code no test reached. That is the "pin
    /// the call site, not the helper" failure this repo has now recorded three times in one
    /// session, and a pure-function test for `addr_differences` does not close it.
    ///
    /// So: two real endpoints, one real connection, then read the verb. It needs a live connection
    /// because that is the only way iroh holds remote state — which is also why this cannot live
    /// inside `peer_diagnostics_never_dials`, whose whole point is that the VERB does not dial. The
    /// dial here is the test's, before the verb is called.
    #[tokio::test(flavor = "multi_thread")]
    async fn known_addrs_reports_irohs_own_view_once_a_connection_exists() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // A second relay-disabled endpoint that accepts, so the connection actually completes and
        // iroh ends up with an OPEN path rather than a half-built one.
        let hermetic = crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            ..Default::default()
        };
        let peer_ep = crate::daemon::boot::build_endpoint(
            iroh::SecretKey::from_bytes(&[31u8; 32]),
            &hermetic,
            false,
            None,
        )
        .await
        .unwrap();
        let peer_id = *peer_ep.id().as_bytes();
        let peer_addr = peer_ep.addr();
        let acceptor = peer_ep.clone();
        let _accept = tokio::spawn(async move {
            while let Some(incoming) = acceptor.accept().await {
                let _ = incoming.await;
            }
        });

        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: peer_id,
                nickname: "jetson".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: Some(serde_json::to_string(&peer_addr).unwrap()),
            })
            .unwrap();

        let _conn = mesh
            .endpoint
            .connect(peer_addr, mcpmesh_net::ALPN_MCP)
            .await
            .expect("loopback dial to a live endpoint");

        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        let known = d.known_addrs.expect(
            "iroh holds remote state for a peer we are connected to right now — `None` here means \
             the field is not wired to `remote_info` at all",
        );
        assert!(
            !known.is_empty(),
            "a live connection has at least one path: {known:?}"
        );
        assert!(
            known.iter().any(|k| k.active),
            "the address carrying this connection must report active — a hardcoded `false` would \
             make the one bit this field exists for meaningless: {known:?}"
        );
        // The labels are the SAME vocabulary the hint uses, which is what makes the two difference
        // lists comparable at all rather than a diff of two formats.
        assert!(
            known
                .iter()
                .all(|k| k.addr.parse::<std::net::SocketAddr>().is_ok()
                    || k.addr.starts_with("relay ")),
            "every label is an IP or a `relay ` entry: {known:?}"
        );
        // And with a real view present, the differences are computed against it rather than
        // short-circuited — the other side of the CRITICAL finding.
        assert!(
            d.hint_addrs_unknown_to_iroh.is_empty(),
            "the hint was built from this very endpoint's address, so iroh holds it: {:?} vs {:?}",
            d.hint_addrs_unknown_to_iroh,
            known
        );
    }

    /// #140: `iroh_addrs_not_in_hint` is a real set difference against a LIVE view — and a
    /// non-empty one is the NORMAL reading on a connected peer, not evidence of a stale hint.
    ///
    /// The test above pins `hint_addrs_unknown_to_iroh` against a real view and asserts nothing
    /// about the converse, so the direction a reader actually reaches for was computed by code no
    /// test observed with a view present. That is the same "pin the call site, not the helper"
    /// shape its own docstring was written to close, left open in the other direction — and the
    /// direction that got misread in the field: the reporter on #140 read `iroh_addrs_not_in_hint`
    /// going 5 -> 8 as the stored hint drifting. It is not. The hint holds the open IP paths of one
    /// live connection; iroh's remote map accumulates every candidate it ever learned. The two have
    /// different cardinality BY DESIGN, and the gap widening means iroh learned more, nothing else.
    ///
    /// Deterministic by construction rather than by what the host's interfaces happen to be: the
    /// hint carries only `192.0.2.7:4433` (TEST-NET-1 — dialable under `is_dialable_addr`, and
    /// never a real local address), while iroh's map holds only what the test's own explicit dial
    /// inserted. The two sets cannot intersect, so both assertions hold on any machine.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_live_view_differences_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let hermetic = crate::config::NetworkCfg {
            relay_mode: "disabled".into(),
            ..Default::default()
        };
        let peer_ep = crate::daemon::boot::build_endpoint(
            iroh::SecretKey::from_bytes(&[32u8; 32]),
            &hermetic,
            false,
            None,
        )
        .await
        .unwrap();
        let peer_id = *peer_ep.id().as_bytes();
        let peer_addr = peer_ep.addr();
        let acceptor = peer_ep.clone();
        let _accept = tokio::spawn(async move {
            while let Some(incoming) = acceptor.accept().await {
                let _ = incoming.await;
            }
        });

        // A well-formed hint for the RIGHT peer, naming an address iroh will never hold. Well-formed
        // matters: an id mismatch or a parse failure degrades to the bare-id dial and empties
        // `hint_addrs`, which would make both assertions below pass for the wrong reason.
        const ONLY_IN_HINT: &str = "192.0.2.7:4433";
        let hint = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            peer_ep.id(),
            [iroh::TransportAddr::Ip(ONLY_IN_HINT.parse().unwrap())],
        ))
        .unwrap();
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: peer_id,
                nickname: "jetson".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: Some(hint),
            })
            .unwrap();

        // The dial names `peer_addr` explicitly, so iroh's remote map gets the peer's REAL
        // addresses and never the hint's. `peer_diagnostics` does not dial, so nothing else can
        // insert `ONLY_IN_HINT` behind the assertions.
        let _conn = mesh
            .endpoint
            .connect(peer_addr, mcpmesh_net::ALPN_MCP)
            .await
            .expect("loopback dial to a live endpoint");

        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        let known = d
            .known_addrs
            .clone()
            .expect("iroh holds remote state for a peer we are connected to right now");
        assert!(
            d.hint_usable,
            "precondition: the hint parses and names THIS peer, so the dial would use it"
        );
        assert_eq!(
            d.hint_addrs,
            vec![ONLY_IN_HINT.to_string()],
            "precondition: the hint contributes exactly the one address iroh cannot hold"
        );

        assert_eq!(
            d.hint_addrs_unknown_to_iroh,
            vec![ONLY_IN_HINT.to_string()],
            "the hint's address is absent from a view that EXISTS — the one case where calling a \
             hint address unknown is correct: {known:?}"
        );
        // The converse, and the point of this test: with a live connection the peer's real
        // addresses are in iroh's view and not in the hint. A reader must not take this for drift.
        assert!(
            !d.iroh_addrs_not_in_hint.is_empty(),
            "iroh holds this connection's real address and the hint does not name it, so the \
             difference must be reported rather than hardcoded empty: {known:?}"
        );
        assert!(
            !d.iroh_addrs_not_in_hint.contains(&ONLY_IN_HINT.to_string()),
            "and it is a genuine difference, not a copy of the view: {:?}",
            d.iroh_addrs_not_in_hint
        );
    }

    /// #140: the hint is forgotten, the DIAL changes, and nothing else about the peer moves.
    ///
    /// The dial assertion is the one that matters. Checking only the stored row would leave the
    /// property the verb exists for — that this pairing now addresses like a fresh identity —
    /// unpinned, which is the failure every gate in this session has found. `stored_dial_addr` is
    /// the function the dial path actually calls, so it is asserted rather than re-derived.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_hint_clear_forgets_the_hint_and_changes_the_dial() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let key = iroh::SecretKey::from_bytes(&[5u8; 32]);
        let eid = *key.public().as_bytes();
        let hint = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            key.public(),
            [iroh::TransportAddr::Ip(
                "192.168.1.50:4433".parse().unwrap(),
            )],
        ))
        .unwrap();
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "jetson".into(),
                services: vec!["kb".into()],
                paired_at: Some("1753000000".into()),
                user_id: Some("b64u:abc".into()),
                last_addr: Some(hint.clone()),
            })
            .unwrap();

        // BEFORE: the dial carries the hint's address.
        let before = crate::daemon::dial::stored_dial_addr(Some(hint.as_str()), key.public());
        assert!(
            !before.addrs.is_empty(),
            "precondition: the hint is being used by the dial"
        );

        let r = peer_hint_clear(&state, "jetson").await.unwrap();
        assert_eq!(r.cleared, 1, "a peer WITH a hint reports one clear");
        assert_eq!(
            r.forgotten,
            vec![hint.clone()],
            "the removed hint is handed back — it is the only undo, since nothing rewrites it on a \
             relay-only pairing and there is no setter"
        );

        let row = mesh.store.resolve(&eid).unwrap().unwrap();
        assert_eq!(row.last_addr, None, "the hint is gone");

        // AFTER: the dial degrades to id-only — exactly what a freshly paired peer does, which is
        // the whole point of the verb.
        let after = crate::daemon::dial::stored_dial_addr(row.last_addr.as_deref(), key.public());
        assert!(
            after.addrs.is_empty(),
            "the dial must now carry NO addresses, so discovery is the only source: {after:?}"
        );
        assert_eq!(after.id, key.public(), "and still name the right peer");

        // Nothing else moved. Asserted field by field: a clear implemented as delete-and-reinsert
        // would leave the row "present" while dropping the pairing proof.
        assert_eq!(row.nickname, "jetson");
        assert_eq!(row.user_id.as_deref(), Some("b64u:abc"));
        assert_eq!(row.services, vec!["kb".to_string()]);
        assert_eq!(
            row.paired_at.as_deref(),
            Some("1753000000"),
            "the pairing stamp is the proof this peer was ever paired — clearing an ADDRESS must \
             not touch it"
        );

        // Idempotent: a second clear is a no-op, and the row survives it.
        let again = peer_hint_clear(&state, "jetson").await.unwrap();
        assert_eq!(again.cleared, 0, "nothing left to clear");
        assert!(again.forgotten.is_empty());
        let row2 = mesh.store.resolve(&eid).unwrap().unwrap();
        assert_eq!(row2.nickname, "jetson");
        assert_eq!(row2.paired_at.as_deref(), Some("1753000000"));

        // An unknown peer ERRORS rather than reporting a successful no-op — an operator running the
        // #140 experiment must not be told they cleared a hint for a peer this node never paired
        // with. All THREE selector forms, because they take different resolution paths and the
        // first version only covered the nickname: `resolve_peer_endpoint`'s `eid:` arm decodes a
        // principal WITHOUT consulting the store, so an unpaired `eid:` returned `cleared: 0` —
        // indistinguishable from a known peer with no hint, and the exact outcome four doc sites
        // claimed was impossible.
        assert!(
            peer_hint_clear(&state, "jetsonn").await.is_err(),
            "a typo'd nickname must not look like a successful clear"
        );
        let stranger = iroh::SecretKey::from_bytes(&[99u8; 32]).public();
        assert!(
            peer_hint_clear(
                &state,
                &format!(
                    "eid:{}",
                    data_encoding::HEXLOWER.encode(stranger.as_bytes())
                )
            )
            .await
            .is_err(),
            "an eid: this node has never paired with must ERROR, not report a clean no-op"
        );
        assert!(
            peer_hint_clear(&state, "b64u:nobody").await.is_err(),
            "a user_id with no stored device must error too"
        );
    }

    /// #140 gate: a `b64u:` selector clears EVERY device of that person, not just the first.
    ///
    /// The first version resolved through `resolve_peer_endpoint`, which returns ONE endpoint — for
    /// a user_id, whichever device sorts first in redb's key order. Since #186 the racing dial path
    /// attaches the stored hint for every device, so clearing one and reporting success left the
    /// pairing still dialling from stored state while telling the operator it now addressed like a
    /// fresh identity. On the two-device fleet in #140 that is the difference between the
    /// experiment working and quietly proving nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_hint_clear_forgets_every_device_of_a_user() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let mut ids = Vec::new();
        for (seed, nick) in [(41u8, "mac"), (42u8, "jetson")] {
            let key = iroh::SecretKey::from_bytes(&[seed; 32]);
            let eid = *key.public().as_bytes();
            let hint = serde_json::to_string(&iroh::EndpointAddr::from_parts(
                key.public(),
                [iroh::TransportAddr::Ip(
                    format!("10.0.0.{seed}:4433").parse().unwrap(),
                )],
            ))
            .unwrap();
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: eid,
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: Some("b64u:sameperson".into()),
                    last_addr: Some(hint),
                })
                .unwrap();
            ids.push(eid);
        }

        let r = peer_hint_clear(&state, "b64u:sameperson").await.unwrap();
        assert_eq!(
            r.cleared, 2,
            "BOTH devices had a hint and both must be cleared"
        );
        assert_eq!(r.forgotten.len(), 2, "and both are handed back: {r:?}");

        for id in &ids {
            assert_eq!(
                mesh.store.resolve(id).unwrap().unwrap().last_addr,
                None,
                "every device of the person must be cleared, not whichever sorted first"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn peer_diagnostics_never_dials() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        for (i, nick) in [(21u8, "jetson"), (22u8, "studio")] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: *iroh::SecretKey::from_bytes(&[i; 32]).public().as_bytes(),
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: None,
                    last_addr: None,
                })
                .unwrap();
        }

        let before = mesh.probe_seq_for_test();
        let d = peer_diagnostics(&state, "jetson").await.unwrap();
        assert_eq!(
            mesh.probe_seq_for_test(),
            before,
            "peer_diagnostics took a probe ticket — it is dialing, and the peer it is meant to be \
             observing is now being perturbed by the act of observing it"
        );
        assert_eq!(
            d.reachability, None,
            "never probed is NOT unreachable — reporting a fabricated `reachable: false` row on a \
             fresh daemon would read as a real verdict in a capture"
        );
    }

    /// #140 gate: the live row is joined by ENDPOINT ID, not by nickname.
    ///
    /// Nicknames collide — that is the entire reason #41/#42/#73 exist, and `add_peer` enforces no
    /// uniqueness. Joining one peer's durable state to a namesake's live row is the most confusing
    /// possible output for a capture whose whole job is comparing two ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_diagnostics_joins_the_live_row_by_id_not_nickname() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let a = *iroh::SecretKey::from_bytes(&[31u8; 32]).public().as_bytes();
        let b = *iroh::SecretKey::from_bytes(&[32u8; 32]).public().as_bytes();
        for eid in [a, b] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: eid,
                    nickname: "jetson".into(), // the SAME name, deliberately
                    services: vec![],
                    paired_at: None,
                    user_id: None,
                    last_addr: None,
                })
                .unwrap();
        }
        // Only B has a live row, and it is reachable.
        mesh.reachability.lock().unwrap().insert(
            b,
            crate::daemon::ReachEntry {
                reachable: true,
                rtt_ms: Some(9),
                probed_at: crate::util::epoch_now_i64(),
                meta: String::new(),
                services: Vec::new(),
                pong_at: Some(crate::util::epoch_now_i64()),
                seq: 1,
                observed: 1,
                path: mcpmesh_local_api::PeerPath::Direct,
            },
        );

        let d = peer_diagnostics(&state, &mcpmesh_net::EndpointId::from_bytes(a).principal())
            .await
            .unwrap();
        assert_eq!(
            d.reachability, None,
            "peer A has no live row of its OWN; borrowing its namesake's would report a direct, \
             9ms link for a peer that has never been probed"
        );
    }

    /// #149: `service_allow_revoke` strips an allow entry by EXACT STRING, so a BARE entry — a
    /// legacy nickname left by a pre-#38 config, a roster group, anything — is removable. The
    /// remedy exists; nothing said so, and nothing pinned it.
    ///
    /// The issue reported bare entries as permanent, having read three code paths that all
    /// genuinely refuse to strip them: `revoke_service_access` says so outright (a nickname-keyed
    /// strip could collide with a group name and revoke a whole roster group),
    /// `write_service_to_config` unions rather than replaces, and this verb's parameter is called
    /// `principal`. That last one is the misreading, and it was a fair one: nothing validates the
    /// argument as a stable principal, and `remove_principal_from_service` compares `s !=
    /// principal`.
    ///
    /// The collision hazard that rules out a nickname-keyed strip does not apply to the STRIP,
    /// which is the whole point: the caller names a LITERAL rather than a name to resolve, so
    /// exactly one line goes and it is the one named. (The SEVER that follows does still resolve —
    /// see `service_allow_revoke`'s rustdoc. This fixture has an empty store and no roster, so it
    /// severs nothing and does not cover that.)
    ///
    /// Pinned across BOTH allow sources — a config entry and an ephemeral registration's
    /// in-memory allow — because "revocation must be fail-closed across every allow the name owns"
    /// (#55 review) applies to a bare entry exactly as it does to a principal.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_exact_literal_revokes_a_bare_allow_entry() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = [\"legacy-nickname\", \"eid:beef\", \"ops-team\"]\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let allow = || {
            crate::config::Config::load(&config_path)
                .unwrap()
                .services
                .get("kb")
                .unwrap()
                .allow
                .clone()
        };

        // The legacy bare entry goes, and ONLY it — an exact match cannot take a neighbour with it.
        service_allow_revoke(&state, "kb".into(), "legacy-nickname".into())
            .await
            .expect("a bare entry is a valid revoke target");
        assert_eq!(
            allow(),
            vec!["eid:beef".to_string(), "ops-team".to_string()],
            "the exact literal is stripped and nothing else is"
        );

        // Idempotent, like every other revoke: a second call is a clean no-op, not an error.
        service_allow_revoke(&state, "kb".into(), "legacy-nickname".into())
            .await
            .expect("revoking an absent entry is a no-op");
        assert_eq!(
            allow(),
            vec!["eid:beef".to_string(), "ops-team".to_string()]
        );

        // A bare entry in an EPHEMERAL registration's in-memory allow is equally removable.
        // Revocation is fail-closed across every allow a name owns (#55 review); a bare entry is
        // not an exception to that.
        mesh.register_ephemeral(
            "tmp".to_string(),
            crate::daemon::EphemeralService {
                backend: mcpmesh_local_api::BackendSpec::Socket {
                    path: "/run/tmp.sock".into(),
                },
                allow: vec!["legacy-nickname".to_string(), "eid:beef".to_string()],
                rate_limit_per_min: None,
            },
        );
        service_allow_revoke(&state, "tmp".into(), "legacy-nickname".into())
            .await
            .expect("a bare entry in an ephemeral allow is a valid target");
        assert_eq!(
            mesh.ephemeral_services
                .lock()
                .unwrap()
                .get("tmp")
                .unwrap()
                .allow,
            vec!["eid:beef".to_string()],
            "the ephemeral allow lost the bare entry and kept the principal"
        );
    }

    /// #149 gate: the exact-match verb has NO multi-device protection, and now that the docs say
    /// so it is pinned.
    ///
    /// `revoke_service_access` (unpair) keeps a shared `b64u:` while another stored peer still
    /// carries it — revoking one device of a person must not revoke the person. This verb
    /// deliberately does not: the caller named the literal, so the literal goes. That asymmetry is
    /// newly documented, and a documented-but-unpinned behaviour is the same defect this whole
    /// issue is about, one level up.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_exact_b64u_revoke_has_no_multi_device_protection() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = [\"b64u:alice\"]\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        // TWO paired devices sharing one person's user_id — the exact fixture the unpair path's
        // guard exists for.
        //
        // `paired_at: Some` is REQUIRED now, and the fixture previously said "paired devices" while
        // leaving it `None` because nothing read it. #65 made the stamp load-bearing: only a device
        // the operator ran a ceremony for counts as a sibling worth protecting, so that an
        // INTRODUCED row — installed on someone else's word — cannot keep a shared user_id alive
        // through an unpairing.
        for (i, nick) in [(7u8, "alice-laptop"), (8u8, "alice-phone")] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: [i; 32],
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: Some("1".into()),
                    user_id: Some("b64u:alice".into()),
                    last_addr: None,
                })
                .unwrap();
        }
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let allow = || {
            crate::config::Config::load(&config_path)
                .unwrap()
                .services
                .get("kb")
                .unwrap()
                .allow
                .clone()
        };

        // Unpairing ONE device leaves the shared user_id alone — the person keeps access.
        revoke_service_access(&mesh, "alice-laptop").await.unwrap();
        assert_eq!(
            allow(),
            vec!["b64u:alice".to_string()],
            "unpairing one device must never revoke the PERSON — the guard this verb lacks"
        );

        // The exact-match verb has no such guard. The caller named the string.
        service_allow_revoke(&state, "kb".into(), "b64u:alice".into())
            .await
            .unwrap();
        assert!(
            allow().is_empty(),
            "an exact literal revoke is exactly as literal as it sounds, shared or not"
        );
    }

    /// #44: `service_allow_grant`/`service_allow_revoke` toggle
    /// service's allow, idempotently, WITHOUT touching peer identity. The "sharing switch".
    #[tokio::test(flavor = "multi_thread")]
    async fn service_allow_grant_and_revoke_toggle_one_principal() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let allow = || {
            crate::config::Config::load(&config_path)
                .unwrap()
                .services
                .get("kb")
                .unwrap()
                .allow
                .clone()
        };

        // Grant → the principal lands in the service allow.
        service_allow_grant(&state, "kb".into(), "eid:beef".into())
            .await
            .unwrap();
        assert_eq!(allow(), vec!["eid:beef".to_string()]);
        // Idempotent grant → still exactly one entry.
        service_allow_grant(&state, "kb".into(), "eid:beef".into())
            .await
            .unwrap();
        assert_eq!(allow(), vec!["eid:beef".to_string()]);

        // Revoke → removed. Peer identity is not involved here at all (no PeerEntry touched).
        service_allow_revoke(&state, "kb".into(), "eid:beef".into())
            .await
            .unwrap();
        assert!(allow().is_empty());
        // Idempotent revoke of an absent principal → clean no-op.
        service_allow_revoke(&state, "kb".into(), "eid:beef".into())
            .await
            .unwrap();
        assert!(allow().is_empty());

        // #55: an unknown service is now an ERROR on both verbs, not a silent success. It used to
        // answer `{}` — which silently included every ephemeral service, whose allow the config
        // writers never touch.
        let grant_err = service_allow_grant(&state, "ghost".into(), "eid:beef".into())
            .await
            .expect_err("an unknown service must not report success");
        assert!(
            grant_err.downcast_ref::<NoSuchService>().is_some(),
            "the grant error must be branchable as NoSuchService, got: {grant_err}"
        );
        let revoke_err = service_allow_revoke(&state, "ghost".into(), "eid:beef".into())
            .await
            .expect_err("an unknown service must not report success");
        assert!(
            revoke_err.downcast_ref::<NoSuchService>().is_some(),
            "the revoke error must be branchable as NoSuchService, got: {revoke_err}"
        );
    }

    /// #55: the no-such-service condition reaches the WIRE as the branchable `-32040`, not the
    /// generic `-32000`. The unit assertions above only prove the Rust-level downcast; a
    /// misordered `respond` arm or a stray `.context()` would silently downgrade the code with
    /// every other test still green.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_service_answers_the_no_such_service_code_on_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh);
        let req = |method: &str| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": method,
                "params": {"service": "ghost", "principal": "eid:beef"}
            })
        };

        for method in ["service_allow_grant", "service_allow_revoke"] {
            let r = crate::control::handle_request(&req(method), &state).await;
            assert_eq!(
                r["error"]["code"],
                mcpmesh_local_api::ERR_NO_SUCH_SERVICE,
                "{method} must answer -32040 for an unknown service, got: {r}"
            );
        }
    }

    /// #55 review: a name held BOTH ephemerally and in config must be revoked from BOTH. The
    /// ephemeral entry shadows the config one in the registry, so stripping only the shadow left
    /// the config allow holding the principal — invisible until the ephemeral registration was
    /// dropped, at which point it went live and re-admitted them.
    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_shadowed_name_strips_the_config_allow_too() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.room]\nsocket = \"/run/room.sock\"\nallow = [\"eid:beef\"]\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        // A hand-edited config under a live ephemeral registration of the same name.
        mesh.register_ephemeral(
            "room".to_string(),
            crate::daemon::EphemeralService {
                backend: mcpmesh_local_api::BackendSpec::Socket {
                    path: "/run/room.sock".into(),
                },
                allow: vec!["eid:beef".to_string()],
                rate_limit_per_min: None,
            },
        );

        revoke_service_allow(&mesh, "room".into(), "eid:beef".into())
            .await
            .unwrap();

        assert!(
            mesh.ephemeral_services
                .lock()
                .unwrap()
                .get("room")
                .unwrap()
                .allow
                .is_empty(),
            "the ephemeral allow is stripped"
        );
        assert!(
            crate::config::Config::load(&config_path)
                .unwrap()
                .services
                .get("room")
                .unwrap()
                .allow
                .is_empty(),
            "the SHADOWED config allow must be stripped too — otherwise it goes live with a \
             revoked principal the moment the ephemeral registration is dropped"
        );
    }

    /// #55 review: a FAILED grant must not leave the in-memory ephemeral allow mutated. The config
    /// write runs first; if it fails the whole grant fails, and a later unrelated reload must not
    /// install a principal the caller was told was not granted.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_config_write_leaves_the_ephemeral_allow_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        // `allow` as a scalar makes `append_allow_to_config` bail.
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = \"not-an-array\"\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        mesh.register_ephemeral(
            "room".to_string(),
            crate::daemon::EphemeralService {
                backend: mcpmesh_local_api::BackendSpec::Socket {
                    path: "/run/room.sock".into(),
                },
                allow: vec![],
                rate_limit_per_min: None,
            },
        );

        let r = grant_service_access(
            &mesh,
            "eid:beef",
            "eid:beef",
            &["room".to_string(), "kb".to_string()],
        )
        .await;
        assert!(r.is_err(), "the malformed config must fail the grant");
        assert!(
            mesh.ephemeral_services
                .lock()
                .unwrap()
                .get("room")
                .unwrap()
                .allow
                .is_empty(),
            "a FAILED grant must not have applied the in-memory half"
        );
    }

    /// #55/#69: both verbs route to an EPHEMERAL registration's in-memory allow, which the config
    /// writers cannot reach. The unit-level complement to `cli/tests/ephemeral_allow.rs` (which
    /// proves a real peer is admitted/refused end to end).
    #[tokio::test(flavor = "multi_thread")]
    async fn allow_verbs_mutate_an_ephemeral_registration() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        mesh.register_ephemeral(
            "room".to_string(),
            crate::daemon::EphemeralService {
                backend: mcpmesh_local_api::BackendSpec::Socket {
                    path: "/run/room.sock".into(),
                },
                allow: vec![],
                rate_limit_per_min: None,
            },
        );
        let allow = || {
            mesh.ephemeral_services
                .lock()
                .unwrap()
                .get("room")
                .unwrap()
                .allow
                .clone()
        };

        service_allow_grant(&state, "room".into(), "eid:beef".into())
            .await
            .unwrap();
        assert_eq!(allow(), vec!["eid:beef".to_string()], "granted in memory");
        service_allow_grant(&state, "room".into(), "eid:beef".into())
            .await
            .unwrap();
        assert_eq!(allow(), vec!["eid:beef".to_string()], "grant is idempotent");

        service_allow_revoke(&state, "room".into(), "eid:beef".into())
            .await
            .unwrap();
        assert!(allow().is_empty(), "revoked in memory");
        service_allow_revoke(&state, "room".into(), "eid:beef".into())
            .await
            .unwrap();
        assert!(allow().is_empty(), "revoke is idempotent");

        // The config service is untouched by the ephemeral routing.
        assert!(
            crate::config::Config::load(&mesh.config_path)
                .unwrap()
                .services
                .get("kb")
                .unwrap()
                .allow
                .is_empty(),
            "an ephemeral grant must not write the config"
        );
    }

    /// The invite registration-check message shapes: silent on all-registered, names the missing
    /// service(s), lists what IS served (matching `status`) or says nothing is served yet, and
    /// always states the exact next command — never wire vocabulary.
    #[test]
    fn unregistered_service_error_message_shapes() {
        let s = |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };

        // Every requested name registered → no error.
        assert_eq!(
            unregistered_service_error(&s(&["notes"]), &s(&["notes", "kb"])),
            None
        );
        // One unknown name, with a served list → name it, list what IS served, point at status.
        assert_eq!(
            unregistered_service_error(&s(&["nosuchsvc"]), &s(&["notes", "code"])).unwrap(),
            "no service named 'nosuchsvc' — you serve: notes, code (see 'mcpmesh status')"
        );
        // Several unknown names → all of them named (the mixed known name is not).
        assert_eq!(
            unregistered_service_error(&s(&["a", "notes", "b"]), &s(&["notes"])).unwrap(),
            "no services named 'a', 'b' — you serve: notes (see 'mcpmesh status')"
        );
        // Nothing served at all → say so, and name the serve command as the next step.
        assert_eq!(
            unregistered_service_error(&s(&["nosuchsvc"]), &[]).unwrap(),
            "no service named 'nosuchsvc' — nothing is served yet; register one with \
             'mcpmesh serve <name> -- <command>'"
        );
    }

    // ---- #172: cancelling an in-flight blob_fetch ------------------------------------------

    /// A mesh with a live app-blob provider — what every cancel test needs, since the registry and
    /// the terminal `Aborted` frame both hang off it.
    async fn blob_mesh() -> (tempfile::TempDir, Arc<MeshState>, Arc<DaemonState>) {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let provider = crate::blobs::provider::AppBlobs::open_fetcher_with_progress(
            dir.path().join("blobs"),
            mesh.endpoint.clone(),
            Some(mesh.blob_bcast_for_test().clone()),
        )
        .await
        .expect("fetcher opens");
        mesh.set_app_blobs(provider).await;
        let state = Arc::new(crate::control::DaemonState::with_mesh("test", mesh.clone()));
        (dir, mesh, state)
    }

    async fn test_provider(mesh: &Arc<MeshState>) -> Arc<crate::blobs::provider::AppBlobs> {
        mesh.app_blobs().await.expect("provider installed")
    }

    fn hex_of(seed: &[u8]) -> String {
        iroh_blobs::Hash::new(seed).to_hex().to_string()
    }

    /// A cancel for a hash nothing is fetching answers `cancelled: false` — NOT an error. It is
    /// the same answer a cancel gets when it loses the race to a fetch that just finished, and
    /// erroring on it would make a UI report a failure for a transfer that in fact succeeded.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_for_a_hash_nothing_is_fetching_is_not_an_error() {
        let (_dir, _mesh, state) = blob_mesh().await;
        let r = blob_fetch_cancel(&state, &hex_of(b"absent")).expect("cancel answers");
        assert!(!r.cancelled, "nothing was in flight");

        // A malformed hash is a caller ERROR, distinct from "nothing in flight". Answering
        // `cancelled: false` for a typo reads as "that transfer already finished" and sends the
        // caller looking in the wrong place while the fetch carries on.
        //
        // Mutation anchor: a raw `HashMap::get(hash)` answers `Ok(cancelled: false)` here.
        assert!(blob_fetch_cancel(&state, "not-a-hash").is_err());

        // And control-only mode still errors rather than panicking, like every other mesh verb.
        assert!(blob_fetch_cancel(&DaemonState::new("test"), &hex_of(b"absent")).is_err());
    }

    /// A cancel stops ONLY the hash it names. Nothing before this test could tell a per-hash
    /// cancel from "stop every transfer on this node".
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_does_not_touch_other_in_flight_fetches() {
        let (_dir, mesh, state) = blob_mesh().await;
        let provider = test_provider(&mesh).await;
        let (one, two) = (hex_of(b"one"), hex_of(b"two"));
        let a = FetchGuard::register(&mesh, one.clone(), provider.clone());
        let provider2 = provider.clone();
        let b = FetchGuard::register(&mesh, two.clone(), provider);

        assert!(blob_fetch_cancel(&state, &one).unwrap().cancelled);
        assert!(a.token.is_cancelled(), "the named hash is cancelled");
        assert!(
            !b.token.is_cancelled(),
            "an unrelated in-flight fetch must be untouched"
        );
        assert!(
            blob_fetch_cancel(&state, &two).unwrap().cancelled,
            "and it is still cancellable on its own"
        );

        // The deterministic half. A lookup that ignores the hash and takes "whatever is in flight"
        // survives the assertions above whenever it happens to pick the named entry; cancelling a
        // hash that is REGISTERED NOWHERE cannot pick the right one, because there isn't one.
        let three = hex_of(b"three");
        let c = FetchGuard::register(&mesh, three, provider2);
        assert!(
            !blob_fetch_cancel(&state, &hex_of(b"absent"))
                .unwrap()
                .cancelled
        );
        assert!(
            !c.token.is_cancelled(),
            "a cancel for an absent hash must trip nothing"
        );
    }

    /// The registry is REFCOUNTED: concurrent fetches of one hash share a token and cancel
    /// together, and only the last one to finish deregisters.
    ///
    /// Mutation anchor: removing the entry unconditionally on drop makes the mid-test cancel
    /// answer `false`, leaving a live sibling fetch uncancellable. A guard that never deregisters
    /// makes the final assertion answer `true` for a fetch that ended.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_fetches_of_one_hash_share_a_token_and_the_last_one_deregisters() {
        let (_dir, mesh, state) = blob_mesh().await;
        let provider = test_provider(&mesh).await;
        let hash = hex_of(b"shared");
        let a = FetchGuard::register(&mesh, hash.clone(), provider.clone());
        let b = FetchGuard::register(&mesh, hash.clone(), provider);
        assert!(
            !a.token.is_cancelled() && !b.token.is_cancelled(),
            "a fresh registration starts live"
        );

        // One fetch finishing must NOT drop a token its sibling is still watching.
        drop(a);
        let r = blob_fetch_cancel(&state, &hash).expect("cancel answers");
        assert!(r.cancelled, "the surviving fetch is still cancellable");
        assert!(b.token.is_cancelled(), "the shared token was tripped");

        drop(b);
        let r = blob_fetch_cancel(&state, &hash).expect("cancel answers");
        assert!(
            !r.cancelled,
            "the last fetch to finish deregisters the hash"
        );
    }

    /// Cancel, then Retry. The retry must NOT inherit the tripped token — the cancelled fetch's
    /// guard has not necessarily unwound yet, and registering against the dead token made the
    /// retry answer "cancelled" instantly for no reason the user could see.
    ///
    /// Mutation anchor: a plain `or_insert_with` (no `is_cancelled` replacement) makes the retry's
    /// token already cancelled here.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_retry_started_before_the_cancelled_fetch_unwinds_gets_a_live_token() {
        let (_dir, mesh, state) = blob_mesh().await;
        let provider = test_provider(&mesh).await;
        let hash = hex_of(b"retry-me");
        let cancelled = FetchGuard::register(&mesh, hash.clone(), provider.clone());
        assert!(blob_fetch_cancel(&state, &hash).unwrap().cancelled);
        assert!(cancelled.token.is_cancelled());

        // The retry starts while the cancelled one is still winding down.
        let retry = FetchGuard::register(&mesh, hash.clone(), provider);
        assert!(
            !retry.token.is_cancelled(),
            "a retry must not inherit a tripped token"
        );
        // The stale registration unwinding must not take the retry's entry with it.
        drop(cancelled);
        assert!(
            blob_fetch_cancel(&state, &hash).unwrap().cancelled,
            "the retry is still registered and cancellable"
        );
        assert!(retry.token.is_cancelled());
    }

    /// A stopped fetch emits a terminal `Aborted` transfer frame (#82 ask 2 + #172). Without it a
    /// subscriber's progress bar sits at its last `Progress` value forever — which is exactly the
    /// UI a Cancel button lives in. Emitted from DROP, so it covers a connection-close abort too,
    /// not only an explicit cancel.
    ///
    /// Mutation anchor: emitting from the `select!`'s cancel arm instead of `Drop` leaves this
    /// assertion (which drops the guard without going through the verb) with no frame.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_fetch_emits_a_terminal_aborted_frame() {
        let (_dir, mesh, _state) = blob_mesh().await;
        let provider = test_provider(&mesh).await;
        let mut rx = mesh.blob_bcast_for_test().subscribe();
        let hash = hex_of(b"stopped");

        let guard = FetchGuard::register(&mesh, hash.clone(), provider.clone());
        drop(guard);
        let frame = rx.try_recv().expect("a terminal frame was broadcast");
        assert_eq!(frame.hash, hash);
        assert_eq!(frame.state, mcpmesh_local_api::BlobTransferState::Aborted);
        assert_eq!(frame.direction, mcpmesh_local_api::BlobDirection::Fetch);

        // A fetch that ANSWERED reports its own terminal state from the provider's progress loop —
        // a second `Aborted` from here would contradict a `Completed` already on the ring.
        let mut finished = FetchGuard::register(&mesh, hash.clone(), provider);
        finished.finished = true;
        drop(finished);
        assert!(
            rx.try_recv().is_err(),
            "a finished fetch must not also report Aborted"
        );
    }

    /// End to end through the real verb: a `blob_fetch` whose provider is unreachable is stopped by
    /// `blob_fetch_cancel` and ANSWERS with [`Cancelled`] — the whole point of cooperative
    /// cancellation over `abort()`, which would deliver no answer at all.
    ///
    /// The ticket names a node that does not exist, so the DIAL is what gets cancelled. That is
    /// deliberate: resolving the hash only from `fetch`'s return value would leave the dial — the
    /// part that can hang longest — outside the cancellable region.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_blob_fetch_answers_cancelled_rather_than_hanging() {
        let (dir, mesh, state) = blob_mesh().await;

        // A well-formed ticket for a node that is not there.
        let nowhere = iroh::SecretKey::from_bytes(&[9u8; 32]).public();
        let hash = iroh_blobs::Hash::new(b"never-fetched");
        // A BLACKHOLE direct address (TEST-NET-3, RFC 5737), so the dial hangs in its QUIC
        // handshake instead of failing instantly. Without it the fetch can be over before a cancel
        // could possibly arrive, and the test would be racing rather than asserting.
        let addr = iroh::EndpointAddr::from_parts(
            nowhere,
            [iroh::TransportAddr::Ip(std::net::SocketAddr::from((
                [203, 0, 113, 1],
                44444,
            )))],
        );
        let ticket = iroh_blobs::ticket::BlobTicket::new(addr, hash, iroh_blobs::BlobFormat::Raw)
            .to_string();
        let hash_hex = hash.to_hex().to_string();

        let dest = dir.path().join("out.bin");
        let fetch_state = state.clone();
        let fetching = tokio::spawn(async move {
            blob_fetch(
                &fetch_state,
                ticket,
                dest.to_string_lossy().into_owned(),
                Vec::new(),
            )
            .await
        });

        // Wait until the fetch has registered itself, so the cancel cannot arrive before it starts.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if mesh
                .fetches
                .lock()
                .expect("fetches lock not poisoned")
                .contains_key(&hash_hex)
            {
                break;
            }
            // Sleep, never `yield_now`: a spin busy-waits a worker thread, and on a small CI box
            // running the suite in parallel that starves the very fetch task it waits for.
            assert!(
                tokio::time::Instant::now() < deadline,
                "the fetch never registered itself"
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let r = blob_fetch_cancel(&state, &hash_hex).expect("cancel answers");
        assert!(r.cancelled, "the in-flight fetch was found and tripped");

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), fetching)
            .await
            .expect("a cancelled fetch answers promptly rather than running to completion")
            .expect("fetch task not panicked");
        let err = outcome.expect_err("a cancelled fetch is an Err, not a silent success");
        assert!(
            err.downcast_ref::<Cancelled>().is_some(),
            "it must be the CANCELLED error, so `respond` codes it ERR_CANCELLED rather than \
             -32000: {err:#}"
        );

        // The guard deregistered on unwind, so a later cancel does not claim a live fetch.
        assert!(!blob_fetch_cancel(&state, &hash_hex).unwrap().cancelled);
    }

    /// #182: a cancelled fetch OWES its subscribers a terminal frame, and did not send one.
    ///
    /// `FetchGuard::drop` emits `Aborted` only when `finished` is false, and `blob_fetch` used to
    /// set that flag after the `select!` — on BOTH arms. On the cancel arm the comment's claim
    /// ("the provider has already reported terminally") is false, because `work` was DROPPED and
    /// nothing inside it emitted anything. So every subscriber watching an explicitly-cancelled
    /// transfer saw `Started`, some `Progress`, and then silence: a progress bar with nothing to
    /// close it, and an embedder keying "transfer over" on a terminal frame waiting forever for a
    /// transfer that had already stopped.
    ///
    /// The #172 cancel test above asserts the error and the deregistration, and never subscribes —
    /// which is why this went unnoticed. Moving `finished = true` back after the select makes this
    /// test time out on the frame.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_fetch_still_emits_a_terminal_frame() {
        let (dir, mesh, state) = blob_mesh().await;
        // Subscribe BEFORE the fetch starts, so no frame can be missed.
        let mut rx = mesh.blob_bcast_for_test().subscribe();

        // Same blackholed address as the test above: the dial hangs in its QUIC handshake, so the
        // fetch is genuinely in flight when the cancel lands rather than already over.
        let nowhere = iroh::SecretKey::from_bytes(&[21u8; 32]).public();
        let hash = iroh_blobs::Hash::new(b"cancel-frame");
        let addr = iroh::EndpointAddr::from_parts(
            nowhere,
            [iroh::TransportAddr::Ip(std::net::SocketAddr::from((
                [203, 0, 113, 1],
                44445,
            )))],
        );
        let ticket = iroh_blobs::ticket::BlobTicket::new(addr, hash, iroh_blobs::BlobFormat::Raw)
            .to_string();
        let hash_hex = hash.to_hex().to_string();

        let dest = dir.path().join("out.bin");
        let fetch_state = state.clone();
        let fetching = tokio::spawn(async move {
            blob_fetch(
                &fetch_state,
                ticket,
                dest.to_string_lossy().into_owned(),
                Vec::new(),
            )
            .await
        });

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if mesh
                .fetches
                .lock()
                .expect("fetches lock not poisoned")
                .contains_key(&hash_hex)
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the fetch never registered itself"
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert!(blob_fetch_cancel(&state, &hash_hex).unwrap().cancelled);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(30), fetching).await;

        // THE assertion: a terminal frame for this hash reaches the ring. Read with a bounded,
        // SLEEPING wait — the ring also carries this fetch's `Started`, so the loop drains until
        // it finds the terminal one or gives up.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut saw_terminal = None;
        while tokio::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(f)
                    if f.hash == hash_hex
                        && matches!(
                            f.state,
                            mcpmesh_local_api::BlobTransferState::Aborted
                                | mcpmesh_local_api::BlobTransferState::Completed
                        ) =>
                {
                    saw_terminal = Some(f.state);
                    break;
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Err(_) => break,
            }
        }
        assert_eq!(
            saw_terminal,
            Some(mcpmesh_local_api::BlobTransferState::Aborted),
            "a cancelled fetch must send a terminal Aborted frame — without it a subscriber's \
             progress bar never closes, and an embedder that keys 'transfer over' on this frame \
             waits forever for a transfer that already stopped"
        );
    }

    /// #182, the other direction: a fetch that ANSWERS must emit EXACTLY ONE terminal frame.
    ///
    /// The guard's `Aborted` exists for a fetch that stopped without answering. If `finished` is
    /// not set on the answering arm, the guard fires on top of the terminal frame the fetch already
    /// sent — so a subscriber sees `Aborted` after a `Completed`, or two `Aborted`s, and a UI that
    /// tears down on the first terminal frame processes a second for a transfer that no longer
    /// exists.
    ///
    /// Driven through `blob_fetch` (the control verb) rather than `AppBlobs::fetch`, because the
    /// guard only exists on the verb path — the acceptance test in `blob_ac.rs` calls the provider
    /// directly and cannot see this at all, which is why the double-emit survived it.
    ///
    /// A fetch that fails on its OWN (no cancel) is the cheap case: on a hermetic mesh with no
    /// discovery, an id-only address fails immediately rather than spending a dial timeout.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_fetch_that_answers_emits_exactly_one_terminal_frame() {
        let (dir, mesh, state) = blob_mesh().await;
        let mut rx = mesh.blob_bcast_for_test().subscribe();

        // Id-only, no direct address: nothing to dial and no discovery, so this fails fast.
        let nowhere = iroh::SecretKey::from_bytes(&[22u8; 32]).public();
        let hash = iroh_blobs::Hash::new(b"answers-once");
        let ticket = iroh_blobs::ticket::BlobTicket::new(
            iroh::EndpointAddr::from(nowhere),
            hash,
            iroh_blobs::BlobFormat::Raw,
        )
        .to_string();
        let hash_hex = hash.to_hex().to_string();

        let dest = dir.path().join("out.bin");
        let r = blob_fetch(
            &state,
            ticket,
            dest.to_string_lossy().into_owned(),
            Vec::new(),
        )
        .await;
        assert!(r.is_err(), "precondition: an undialable source fails");

        // Drain everything the ring holds and COUNT terminal frames for this hash.
        let mut terminal = 0usize;
        loop {
            match rx.try_recv() {
                Ok(f)
                    if f.hash == hash_hex
                        && matches!(
                            f.state,
                            mcpmesh_local_api::BlobTransferState::Aborted
                                | mcpmesh_local_api::BlobTransferState::Completed
                        ) =>
                {
                    terminal += 1;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert_eq!(
            terminal, 1,
            "a fetch that answered must emit ONE terminal frame — the guard's Aborted is for a \
             fetch that stopped WITHOUT answering, and firing it here means a subscriber tears \
             down twice for one transfer"
        );
    }

    /// A bare endpoint on the app-blob ALPN that COUNTS completed handshakes and serves nothing, so
    /// whether a fetch dialled it is observable (#223).
    async fn counting_blob_source() -> (
        iroh::Endpoint,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![crate::blobs::APP_BLOB_ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (e, c) = (ep.clone(), n.clone());
        let task = tokio::spawn(async move {
            while let Some(incoming) = e.accept().await {
                if incoming.await.is_ok() {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });
        (ep, n, task)
    }

    /// A paired row for `ep` carrying its REAL address as the dial hint, so a dial to it on this
    /// hermetic mesh actually lands — which is what makes a zero count mean "not dialled".
    fn seed_source_row(mesh: &MeshState, nickname: &str, ep: &iroh::Endpoint) {
        let addr = ep.addr();
        assert!(
            !addr.addrs.is_empty(),
            "fixture: the source must have a dialable address"
        );
        mesh.store
            .add(PeerEntry {
                endpoint_id: *ep.id().as_bytes(),
                nickname: nickname.into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: Some(serde_json::to_string(&addr).unwrap()),
            })
            .unwrap();
    }

    /// Wait (bounded, sleeping) until `n` is non-zero.
    async fn await_count(n: &std::sync::atomic::AtomicUsize, what: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while n.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(tokio::time::Instant::now() < deadline, "{what}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// #223: a `blob_fetch` naming a REVOKED device as an alternate source never contacts it.
    ///
    /// `blob_source_addrs` resolved through `protocol_candidates`, which applied no revocation
    /// filter, so a fetch dialled a device this node had revoked and asked it for the blob. Both
    /// sources are real, reachable endpoints that count handshakes, named in revoked-first order so
    /// an unfiltered fetch dials the stolen one before the live one: the LIVE count proves the fetch
    /// reached its alternates at all, so the stolen count of zero means "filtered", not "never got
    /// that far". The publisher is an id-only ticket that fails at once on a hermetic mesh.
    ///
    /// Deleting the `dial_refused` filter in `protocol_candidates` fails the zero-count assertion
    /// and the source count; dropping the `skipped_revoked` context in `blob_fetch` fails the
    /// "named source skipped" assertion.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_blob_fetch_never_dials_a_revoked_alternate_source() {
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let (dir, mesh, state) = blob_mesh().await;
            let (stolen_ep, stolen, _t1) = counting_blob_source().await;
            let (live_ep, live, _t2) = counting_blob_source().await;
            seed_source_row(&mesh, "stolen", &stolen_ep);
            seed_source_row(&mesh, "live", &live_ep);
            mesh.store
                .revoke(crate::allowlist::RevokedEntry {
                    endpoint_id: *stolen_ep.id().as_bytes(),
                    revoked_at: 1,
                    reason: None,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                })
                .unwrap();

            let nowhere = iroh::SecretKey::from_bytes(&[23u8; 32]).public();
            let ticket = iroh_blobs::ticket::BlobTicket::new(
                iroh::EndpointAddr::from(nowhere),
                iroh_blobs::Hash::new(b"revoked-source"),
                iroh_blobs::BlobFormat::Raw,
            )
            .to_string();
            let err = blob_fetch(
                &state,
                ticket,
                dir.path().join("out.bin").to_string_lossy().into_owned(),
                vec!["stolen".into(), "live".into()],
            )
            .await
            .expect_err("nobody serves this blob");
            await_count(&live, "control: the live alternate must be dialled").await;
            // A stray dial to the stolen source would have landed well within this.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            assert_eq!(
                stolen.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "a revoked source must never be contacted by a blob fetch"
            );
            assert!(
                format!("{err:#}").contains("tried 2 sources"),
                "the publisher and the live alternate only — the revoked one is not a source: \
                 {err:#}"
            );
            // …and the caller, who NAMED `stolen`, is told why it was not tried (#223 review).
            assert!(
                format!("{err:#}").contains("1 named source skipped: revoked on this node"),
                "{err:#}"
            );
        })
        .await
        .expect("revoked-source test timed out");
    }

    /// #223 (review): a ticket whose PUBLISHER is a device this node revoked must not make the fetch
    /// contact it. The alternates were filtered, and source 0 — the ticket's own address, a remote
    /// party's claim — was dialled unconditionally.
    ///
    /// The revoked publisher carries its REAL address in the ticket, so an unfiltered fetch lands on
    /// it; the live alternate is the control that the fetch ran at all. With every source refused
    /// the fetch fails as an exhausted one does, having dialled nothing.
    ///
    /// Passing `with_publisher: true` unconditionally in `blob_fetch` fails the zero count and the
    /// source counts.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_blob_fetch_never_dials_a_revoked_publisher() {
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let (dir, mesh, state) = blob_mesh().await;
            let (stolen_ep, stolen, _t1) = counting_blob_source().await;
            let (live_ep, live, _t2) = counting_blob_source().await;
            seed_source_row(&mesh, "stolen", &stolen_ep);
            seed_source_row(&mesh, "live", &live_ep);
            mesh.store
                .revoke(crate::allowlist::RevokedEntry {
                    endpoint_id: *stolen_ep.id().as_bytes(),
                    revoked_at: 1,
                    reason: None,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                })
                .unwrap();
            let ticket = iroh_blobs::ticket::BlobTicket::new(
                stolen_ep.addr(),
                iroh_blobs::Hash::new(b"revoked-publisher"),
                iroh_blobs::BlobFormat::Raw,
            )
            .to_string();
            let dest = || dir.path().join("out.bin").to_string_lossy().into_owned();

            let err = blob_fetch(&state, ticket.clone(), dest(), vec!["live".into()])
                .await
                .expect_err("nobody serves this blob");
            await_count(&live, "control: the live alternate must be dialled").await;
            assert!(
                format!("{err:#}").contains("tried 1 source)"),
                "only the live alternate is a source: {err:#}"
            );

            // Every source refused: the fetch fails as an exhausted one, contacting nobody.
            let err = blob_fetch(&state, ticket, dest(), vec!["stolen".into()])
                .await
                .expect_err("no dialable source");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("no source to try") && msg.contains("tried 0 sources"),
                "{msg}"
            );
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            assert_eq!(
                stolen.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "a revoked publisher must never be contacted by a blob fetch"
            );
        })
        .await
        .expect("revoked-publisher test timed out");
    }

    /// The blob control operations fail gracefully (Err, never a panic) in control-only mode — the
    /// `state.mesh()` guard every one shares before touching the app-blob provider.
    #[tokio::test]
    async fn blob_ops_error_without_a_mesh() {
        let st = DaemonState::new("test");
        assert!(blob_list(&st, Default::default()).await.is_err());
        assert!(
            blob_publish(&st, "scope".into(), "/tmp/x".into())
                .await
                .is_err()
        );
        assert!(blob_grant(&st, "scope".into(), "bob".into()).await.is_err());
        assert!(
            blob_fetch(&st, "ticket".into(), "/tmp/dst".into(), Vec::new())
                .await
                .is_err()
        );
    }

    /// The typed `peer_rename` params, as the control dispatcher hands them to `rename_peer`.
    fn rename_params(user_id: Option<&str>, to: &str) -> PeerRenameParams {
        PeerRenameParams {
            user_id: user_id.map(str::to_string),
            nickname: None,
            to: to.into(),
        }
    }

    /// `rename_peer` renames ALL of a person's devices (matched by user_id) to the new nickname —
    /// and touches NOTHING else (#38): `allow` holds stable principals (here the renamed person's
    /// own `b64u:` user_id), so the config is byte-identical after the rename. Grants survive a
    /// rename by construction — no allow rewrite happens because no grant names a nickname.
    #[tokio::test]
    async fn rename_peer_renames_all_devices_and_leaves_grants_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        // The grant names the renamed person's PRINCIPAL — the strongest case: even the
        // renamed person's own grant must not be rewritten.
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = [\"b64u:BOB\"]\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        // Two devices of ONE person (same user_id), both under the old nickname.
        mesh.store
            .add(rename_entry(1, "bob-old", Some("b64u:BOB")))
            .unwrap();
        mesh.store
            .add(rename_entry(2, "bob-old", Some("b64u:BOB")))
            .unwrap();
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let config_before = std::fs::read_to_string(&config_path).unwrap();

        rename_peer(&state, rename_params(Some("b64u:BOB"), "Bobby"))
            .await
            .unwrap();

        // Both PeerEntries now carry the new nickname.
        let names: Vec<String> = mesh
            .store
            .list()
            .unwrap()
            .into_iter()
            .map(|e| e.nickname)
            .collect();
        assert!(
            names.iter().all(|n| n == "Bobby"),
            "all devices renamed, got {names:?}"
        );
        // #38: the rename touched ONLY nicknames — the config (and its principal-keyed allow)
        // is byte-identical, so the grant survived without any rewrite.
        let config_after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(
            config_before, config_after,
            "rename must not rewrite the config"
        );
        let doc: toml::Table = toml::from_str(&config_after).unwrap();
        let allow = doc["services"]["kb"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 1);
        assert_eq!(allow[0].as_str(), Some("b64u:BOB"));
    }

    /// `rename_peer` rejects an empty nickname, a request that names no contact, a no-such-contact
    /// target, and a collision onto ANOTHER contact's nickname (the impersonation guard) — and on a
    /// rejected rename nothing changes.
    #[tokio::test]
    async fn rename_peer_guards_bad_requests_and_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path).await;
        mesh.store
            .add(rename_entry(1, "alice", Some("b64u:ALICE")))
            .unwrap();
        mesh.store
            .add(rename_entry(2, "bob", Some("b64u:BOB")))
            .unwrap();
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // Empty `to` (whitespace trims to empty).
        assert!(
            rename_peer(&state, rename_params(Some("b64u:ALICE"), "  "))
                .await
                .is_err()
        );
        // Neither user_id nor nickname identifies a contact.
        assert!(rename_peer(&state, rename_params(None, "X")).await.is_err());
        // No matching contact.
        assert!(
            rename_peer(&state, rename_params(Some("b64u:NOBODY"), "X"))
                .await
                .is_err()
        );
        // Collision: renaming alice onto bob's nickname would steal bob's identity/grants.
        assert!(
            rename_peer(&state, rename_params(Some("b64u:ALICE"), "bob"))
                .await
                .is_err()
        );
        // The guard held: nothing changed — alice is still "alice", bob still "bob".
        let names: std::collections::BTreeSet<String> = mesh
            .store
            .list()
            .unwrap()
            .into_iter()
            .map(|e| e.nickname)
            .collect();
        assert!(
            names.contains("alice") && names.contains("bob"),
            "no rename should have occurred: {names:?}"
        );
    }

    fn rename_entry(id: u8, nickname: &str, user_id: Option<&str>) -> PeerEntry {
        PeerEntry {
            endpoint_id: [id; 32],
            nickname: nickname.into(),
            services: Vec::new(),
            paired_at: None,
            user_id: user_id.map(str::to_string),
            last_addr: None,
        }
    }

    #[test]
    fn rename_plan_groups_by_user_id_and_guards_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let store = PeerStore::open(&dir.path().join("s.redb")).unwrap();
        store
            .add(rename_entry(1, "bob-phone", Some("b64u:BOB")))
            .unwrap();
        store
            .add(rename_entry(2, "bob-laptop", Some("b64u:BOB")))
            .unwrap();
        store
            .add(rename_entry(3, "carol", Some("b64u:CAROL")))
            .unwrap();

        // Renaming the PERSON by user_id targets BOTH of Bob's devices in one op.
        let plan = rename_plan(&store, Some("b64u:BOB"), None, "Bobby")
            .unwrap()
            .unwrap();
        assert_eq!(plan.targets.len(), 2);

        // GUARD (a) display-uniqueness: renaming Bob → "carol" (a DIFFERENT contact) is
        // refused — a duplicate display name misdirects outbound dials. (The old orphan-allow
        // guard (b) is GONE, #38: allow holds principals, so no name can inherit a grant.)
        assert!(rename_plan(&store, Some("b64u:BOB"), None, "carol").is_err());
        // A provisional contact (no user_id) renames by nickname to a fresh name.
        store.add(rename_entry(4, "dave", None)).unwrap();
        assert_eq!(
            rename_plan(&store, None, Some("dave"), "Dave")
                .unwrap()
                .unwrap()
                .targets
                .len(),
            1
        );
        // Renaming to the current name is a no-op (Ok(None)).
        assert!(
            rename_plan(&store, Some("b64u:CAROL"), None, "carol")
                .unwrap()
                .is_none()
        );
        // No matching contact → error.
        assert!(rename_plan(&store, Some("b64u:NOBODY"), None, "x").is_err());
    }

    /// #85 ask 4: a signed revocation is honoured ONLY from someone we already trust, and ONLY for
    /// their OWN device.
    ///
    /// The middle case is the one that matters and the one a plausible implementation gets wrong.
    /// A signature proves WHO ASKED; it does not prove the endpoint was ever theirs. Without the
    /// second check, any peer you have paired with could mark a THIRD party's device dead in your
    /// node — turning "you may kill your own devices here" into a denial-of-service primitive
    /// against everyone else you know.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_signed_revocation_is_scoped_to_the_signers_own_devices() {
        use mcpmesh_trust::UserKey;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // Bob (paired, with a proven user_id) and Carol (paired, different person).
        let bob_key = UserKey::from_signing_key(
            mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&[41u8; 32]),
        );
        let bob_uid = mcpmesh_trust::binding::user_id(&bob_key);
        let stranger = UserKey::from_signing_key(
            mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]),
        );
        let (bob_laptop, bob_phone, carol_laptop) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        for (eid, nick, uid) in [
            (bob_laptop, "bob-laptop", Some(bob_uid.clone())),
            (bob_phone, "bob-phone", Some(bob_uid.clone())),
            (carol_laptop, "carol", Some("b64u:CAROL".to_string())),
        ] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: eid,
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: uid,
                    last_addr: None,
                })
                .unwrap();
        }

        let token = |key: &UserKey, target: [u8; 32], issued_at: u64| {
            let sig = mcpmesh_trust::roster::sign::sign_device_revocation(
                key.signing_key(),
                &target,
                issued_at,
            );
            let t = RevocationToken {
                user_id: mcpmesh_trust::binding::user_id(key),
                endpoint: mcpmesh_net::EndpointId::from_bytes(target).principal(),
                issued_at,
                sig: mcpmesh_trust::roster::encode_b64u(&sig),
                reason: None,
            };
            format!(
                "{REVOKE_SCHEME}{}",
                data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&t).unwrap())
            )
        };
        let import = async |tok: String| {
            device_revocation_import(
                &state,
                mcpmesh_local_api::DeviceRevocationImportParams { token: tok },
            )
            .await
        };

        // (a) A STRANGER's signature — valid crypto, unknown signer.
        let e = import(token(&stranger, carol_laptop, 100))
            .await
            .expect_err("a stranger's revocation must be refused");
        assert!(
            format!("{e:#}").contains("does not know"),
            "the refusal must name the reason: {e:#}"
        );
        assert!(
            !mesh.store.is_revoked(&carol_laptop),
            "and must have changed nothing"
        );

        // (b) A TRUSTED peer revoking a THIRD PARTY's device. This is the weapon case.
        let e = import(token(&bob_key, carol_laptop, 100))
            .await
            .expect_err("bob must not be able to revoke carol's device in our node");
        assert!(
            format!("{e:#}").contains("never a third party"),
            "the refusal must say why: {e:#}"
        );
        assert!(
            !mesh.store.is_revoked(&carol_laptop),
            "carol's device must still be live — a paired peer is not an authority over everyone \
             else you know"
        );

        // (c) A trusted peer revoking their OWN device: applied.
        let r = import(token(&bob_key, bob_laptop, 100))
            .await
            .expect("bob may revoke bob's device");
        assert!(r.applied);
        assert_eq!(r.user_id, bob_uid);
        assert!(mesh.store.is_revoked(&bob_laptop));
        assert!(
            !mesh.store.is_revoked(&bob_phone),
            "…and only the device NAMED — revoking one device is not revoking the person"
        );

        // (d) Replaying an OLDER token must not displace the newer statement, and re-importing the
        // same one is a clean no-op. `issued_at` is inside the signature, so it cannot be re-dated
        // to win this comparison.
        let r = import(token(&bob_key, bob_laptop, 200))
            .await
            .expect("a newer statement applies");
        assert!(r.applied);
        for stale in [50u64, 100, 200] {
            let r = import(token(&bob_key, bob_laptop, stale))
                .await
                .expect("an older or equal token is accepted and ignored, not an error");
            assert!(
                !r.applied,
                "issued_at={stale} must not displace the newer revocation"
            );
        }
        assert_eq!(
            mesh.store
                .revoked_entry(&bob_laptop)
                .unwrap()
                .unwrap()
                .issued_at,
            Some(200),
            "the newest statement survives every replay"
        );

        // (e) A tampered signature is refused before any store read.
        let mut bad = token(&bob_key, bob_phone, 300);
        bad.push('x');
        assert!(import(bad).await.is_err());
        assert!(!mesh.store.is_revoked(&bob_phone));
    }

    /// #85 ask 4: an endpoint this node has NEVER SEEN is refused, not recorded.
    ///
    /// **This test asserted the opposite until the 0.45.0 gate**, and in doing so it certified a
    /// denial-of-service primitive: endpoint ids are public (invite lines, `status`, dial hints),
    /// so any paired peer could sign a revocation of an endpoint we had not met yet and kill it
    /// permanently on arrival — the pairing would complete, the gate would refuse it, and the only
    /// remedy would be an operator running `revoke undo` on a principal they never revoked. The
    /// gate proved it end to end.
    ///
    /// We cannot verify ownership of an endpoint we hold no row for, and a binding carried in the
    /// token would not help: the signer could bind any endpoint to themselves, which is the same
    /// lie wearing a signature. So the answer is to refuse, and to say what to do instead.
    ///
    /// The cost is real and is not hidden: a peer who pairs with the stolen device AFTER receiving
    /// the token must import it again once the row exists.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_endpoint_we_have_never_seen_is_refused_not_pre_revoked() {
        use mcpmesh_trust::UserKey;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let bob = UserKey::from_signing_key(mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(
            &[41u8; 32],
        ));
        let bob_uid = mcpmesh_trust::binding::user_id(&bob);
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: [1u8; 32],
                nickname: "bob-phone".into(),
                services: vec![],
                paired_at: None,
                user_id: Some(bob_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        let stolen = [77u8; 32]; // never paired here
        let sig =
            mcpmesh_trust::roster::sign::sign_device_revocation(bob.signing_key(), &stolen, 10);
        let t = RevocationToken {
            user_id: bob_uid,
            endpoint: mcpmesh_net::EndpointId::from_bytes(stolen).principal(),
            issued_at: 10,
            sig: mcpmesh_trust::roster::encode_b64u(&sig),
            reason: Some("stolen".into()),
        };
        let token = format!(
            "{REVOKE_SCHEME}{}",
            data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&t).unwrap())
        );

        let e = device_revocation_import(
            &state,
            mcpmesh_local_api::DeviceRevocationImportParams { token },
        )
        .await
        .expect_err("a revocation for an endpoint we have never seen must be REFUSED");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("no record of"),
            "the refusal must say why: {msg}"
        );
        assert!(
            msg.contains("Pair with them first"),
            "…and what to do instead, or the operator is stuck with a token that does nothing: \
             {msg}"
        );
        assert!(
            !mesh.store.is_revoked(&stolen),
            "nothing may be recorded — a held revocation for an unmet endpoint is the DoS"
        );
        assert!(
            mesh.store.list_revoked().unwrap().is_empty(),
            "…not even as an inert row"
        );

        // And the case that IS the ask still works: once the device is known to be theirs.
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: stolen,
                nickname: "bob-laptop".into(),
                services: vec![],
                paired_at: None,
                user_id: Some(mcpmesh_trust::binding::user_id(&bob)),
                last_addr: None,
            })
            .unwrap();
        let sig2 =
            mcpmesh_trust::roster::sign::sign_device_revocation(bob.signing_key(), &stolen, 10);
        let t2 = RevocationToken {
            user_id: mcpmesh_trust::binding::user_id(&bob),
            endpoint: mcpmesh_net::EndpointId::from_bytes(stolen).principal(),
            issued_at: 10,
            sig: mcpmesh_trust::roster::encode_b64u(&sig2),
            reason: Some("stolen".into()),
        };
        let r = device_revocation_import(
            &state,
            mcpmesh_local_api::DeviceRevocationImportParams {
                token: format!(
                    "{REVOKE_SCHEME}{}",
                    data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&t2).unwrap())
                ),
            },
        )
        .await
        .expect("once we know the device is theirs, the same token applies");
        assert!(r.applied);
        assert!(
            mesh.gate.resolve(&stolen.into()).is_none(),
            "the live gate refuses it through the composition, not just the store"
        );
    }

    /// #85 ask 4: a `b64u:` revokes EVERY device of that person, and ONLY those.
    ///
    /// The contract an operator reaches for when the answer is "not on any of their machines" —
    /// promised in three places and, until the 0.45.0 gate, tested nowhere: `revocation_targets`
    /// mutated to `.take(1)` left the whole suite green while reporting success and leaving the
    /// other devices live.
    ///
    /// Also pins the negative half. Over-revoking would be a self-inflicted outage against someone
    /// the operator never named.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_id_revokes_every_device_of_that_person_and_only_those() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (bob1, bob2, bob3, carol) = ([1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]);
        for (eid, nick, uid) in [
            (bob1, "bob-laptop", "b64u:BOB"),
            (bob2, "bob-phone", "b64u:BOB"),
            (bob3, "bob-tablet", "b64u:BOB"),
            (carol, "carol", "b64u:CAROL"),
        ] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: eid,
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: Some(uid.into()),
                    last_addr: None,
                })
                .unwrap();
        }

        let out = peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "b64u:BOB".into(),
                reason: None,
            },
        )
        .await
        .expect("revoking a person succeeds");
        assert_eq!(
            out.revoked.len(),
            3,
            "all THREE of bob's devices must be revoked, not just the first: {:?}",
            out.revoked
        );
        for eid in [bob1, bob2, bob3] {
            assert!(mesh.store.is_revoked(&eid));
        }
        assert!(
            !mesh.store.is_revoked(&carol),
            "carol must be untouched — over-revoking is a self-inflicted outage"
        );
    }

    /// A `peer_revoke` naming nothing is an ERROR, never an empty success.
    ///
    /// This is a revocation surface: a typo reading as "done" leaves an operator believing they cut
    /// off a device they never touched. Untested until the gate deleted the guard and everything
    /// stayed green.
    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_name_that_matches_nothing_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let e = peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "nobody".into(),
                reason: None,
            },
        )
        .await
        .expect_err("a name matching no device must be refused, not reported as success");
        assert!(
            format!("{e:#}").contains("nobody"),
            "the error must name the value so a typo is fixable: {e:#}"
        );
    }

    /// `peer_unrevoke` must still work once the pair row is gone.
    ///
    /// Revoke, then unpair: nothing resolves the principal any more, so without the bare-`eid:`
    /// fallback the revocation becomes permanently unliftable — a trap an operator falls into by
    /// doing the two obvious things in the obvious order.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revocation_stays_liftable_after_the_pair_row_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let eid = [8u8; 32];
        let principal = mcpmesh_net::EndpointId::from_bytes(eid).principal();
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "gone".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();
        peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "gone".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
        mesh.store.remove("gone").unwrap();
        assert!(
            mesh.store.is_revoked(&eid),
            "precondition: still revoked after the row is deleted"
        );

        let out = peer_unrevoke(
            &state,
            mcpmesh_local_api::PeerUnrevokeParams {
                peer: principal.clone(),
            },
        )
        .await
        .expect("a bare eid: must still lift the revocation");
        assert_eq!(out.unrevoked, vec![principal]);
        assert!(!mesh.store.is_revoked(&eid));
    }

    /// A LOCAL revoke must not overwrite a SIGNED one.
    ///
    /// Two things break if it does: `status` stops showing that the device's OWNER declared it
    /// dead — strictly stronger evidence than the operator's own note — and clearing `issued_at`
    /// disarms the replay guard, so an OLDER token from that peer re-applies and wins.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_revoke_does_not_destroy_a_signed_revocations_provenance() {
        use mcpmesh_trust::UserKey;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let bob = UserKey::from_signing_key(mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(
            &[41u8; 32],
        ));
        let bob_uid = mcpmesh_trust::binding::user_id(&bob);
        let eid = [1u8; 32];
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "bob-laptop".into(),
                services: vec![],
                paired_at: None,
                user_id: Some(bob_uid.clone()),
                last_addr: None,
            })
            .unwrap();
        let sig = mcpmesh_trust::roster::sign::sign_device_revocation(bob.signing_key(), &eid, 500);
        let t = RevocationToken {
            user_id: bob_uid.clone(),
            endpoint: mcpmesh_net::EndpointId::from_bytes(eid).principal(),
            issued_at: 500,
            sig: mcpmesh_trust::roster::encode_b64u(&sig),
            reason: None,
        };
        device_revocation_import(
            &state,
            mcpmesh_local_api::DeviceRevocationImportParams {
                token: format!(
                    "{REVOKE_SCHEME}{}",
                    data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&t).unwrap())
                ),
            },
        )
        .await
        .expect("import the owner's statement");

        peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "bob-laptop".into(),
                reason: Some("also me".into()),
            },
        )
        .await
        .expect("a local revoke on top succeeds");

        let row = mesh.store.revoked_entry(&eid).unwrap().unwrap();
        assert_eq!(
            (
                row.source.as_str(),
                row.signer_user_id.as_deref(),
                row.issued_at
            ),
            ("signed", Some(bob_uid.as_str()), Some(500)),
            "the OWNER's statement must survive a local revoke on top of it — losing it downgrades \
             the evidence and re-arms the replay an older token would win"
        );
    }

    /// #85 ask 4: revocation must block the OUTBOUND direction too.
    ///
    /// The first cut was inbound-only — the gate refused a revoked device's connections, but every
    /// outbound path still read `PeerStore` directly and connected to the machine the operator had
    /// just declared stolen, handing it the request. The 0.45.0 gate proved it.
    ///
    /// That made the weaker verb stronger: `peer_remove` DELETES the row, so it blocked the dial;
    /// `peer_revoke`, the compromise claim, did not. Backwards, and in the direction that leaks
    /// data to whoever holds the device.
    ///
    /// All three selector shapes are exercised, because the dial path resolves each one
    /// differently and a guard on only some of them is the shape of the original bug.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_peer_is_refused_on_the_outbound_path_by_every_selector() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let eid = [6u8; 32];
        let principal = mcpmesh_net::EndpointId::from_bytes(eid).principal();
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "stolen-laptop".into(),
                services: vec!["notes".into()],
                paired_at: None,
                user_id: Some("b64u:BOB".into()),
                last_addr: None,
            })
            .unwrap();

        // Precondition: all three selectors resolve BEFORE the revoke, or the assertions below
        // could pass on a resolver that never worked.
        for sel in ["stolen-laptop", "b64u:BOB", principal.as_str()] {
            resolve_peer_endpoint(&mesh, sel).await.unwrap_or_else(|e| {
                panic!("precondition: {sel} resolves before revocation: {e:#}")
            });
        }

        peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "stolen-laptop".into(),
                reason: None,
            },
        )
        .await
        .unwrap();

        // The shared resolver behind `peer_services` / `peer_diagnostics`, which both DIAL.
        for sel in ["stolen-laptop", "b64u:BOB", principal.as_str()] {
            let e = resolve_peer_endpoint(&mesh, sel).await.expect_err(
                "a revoked peer must not be resolved for a dial — that hands the request to the \
                 device the operator declared compromised",
            );
            assert!(
                format!("{e:#}").contains("REVOKED"),
                "the refusal must say why, for {sel}: {e:#}"
            );
        }

        // And the session dial itself, by nickname and by device principal.
        for sel in ["stolen-laptop", principal.as_str()] {
            let e = match crate::daemon::dial::dial_service(&mesh, sel, "notes").await {
                Ok(_) => panic!("dial_service must refuse a revoked peer ({sel})"),
                Err(e) => e,
            };
            assert!(
                format!("{e:#}").contains("REVOKED"),
                "dial refusal must say why, for {sel}: {e:#}"
            );
        }

        // Lifting it restores the outbound path, which is what makes the guard a gate and not a
        // one-way door.
        peer_unrevoke(
            &state,
            mcpmesh_local_api::PeerUnrevokeParams {
                peer: "stolen-laptop".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            resolve_peer_endpoint(&mesh, "stolen-laptop").await.is_ok(),
            "unrevoking must restore the outbound path too"
        );
    }

    /// #218: an IDENTITY revocation blocks the OUTBOUND direction for every device carrying it, on
    /// the endpoint table's own terms — including a device whose endpoint row was lifted by a
    /// per-device `peer_unrevoke`. The gate refuses that device inbound on the identity alone, so a
    /// dial that still went out would be #85 ask 4's backwards verb again: the claim "not on any of
    /// their machines" enforced one way, and the request handed over the other.
    ///
    /// Fixture: two devices of `b64u:mallory`, revoked as a person, then the laptop lifted by
    /// nickname — its endpoint row is gone (asserted), the identity row stands (asserted). The
    /// phone keeps its endpoint row, so the `b64u:` race has nothing to fall back on. The inverse:
    /// `peer_unrevoke` on the identity restores the outbound path for both.
    ///
    /// Each outbound site reverted to `PeerStore::is_revoked` fails exactly one group: the `eid:`
    /// arm of `resolve_peer_endpoint` (the `eid:` resolve), its tail (the nickname and `b64u:`
    /// resolves), `refuse_if_revoked` (the nickname and `eid:` session dials), the `hinted_addrs`
    /// filter (the `b64u:` race).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_identity_is_refused_on_the_outbound_path_even_after_a_device_unrevoke() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let (laptop, phone) = ([5u8; 32], [6u8; 32]);
        let laptop_eid = mcpmesh_net::EndpointId::from_bytes(laptop).principal();
        for (eid, nickname) in [(laptop, "mallory-laptop"), (phone, "mallory-phone")] {
            mesh.store
                .add(PeerEntry {
                    endpoint_id: eid,
                    nickname: nickname.into(),
                    services: vec!["notes".into()],
                    paired_at: None,
                    user_id: Some("b64u:mallory".into()),
                    last_addr: None,
                })
                .unwrap();
        }
        let unrevoke = |peer: &str| {
            peer_unrevoke(
                &state,
                mcpmesh_local_api::PeerUnrevokeParams { peer: peer.into() },
            )
        };
        peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "b64u:mallory".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
        unrevoke("mallory-laptop").await.unwrap();
        assert!(
            !mesh.store.is_revoked(&laptop),
            "fixture: the laptop's endpoint row is lifted"
        );
        assert!(
            mesh.store.is_user_revoked("b64u:mallory"),
            "fixture: the identity row stands"
        );
        assert!(
            mesh.gate.resolve(&laptop.into()).is_none(),
            "fixture: the gate refuses the laptop inbound"
        );

        for sel in ["mallory-laptop", laptop_eid.as_str()] {
            let e = resolve_peer_endpoint(&mesh, sel)
                .await
                .expect_err("a device of a revoked identity must not be resolved for a dial");
            assert!(format!("{e:#}").contains("REVOKED"), "{sel}: {e:#}");
            let e = match crate::daemon::dial::dial_service(&mesh, sel, "notes").await {
                Ok(_) => panic!("dial_service must refuse a device of a revoked identity ({sel})"),
                Err(e) => e,
            };
            assert!(format!("{e:#}").contains("REVOKED"), "{sel}: {e:#}");
        }
        // By identity: the shared resolver picks the first device (the laptop), and the session
        // dial RACES both — so the race filter must drop the laptop too (the phone is still
        // endpoint-revoked, leaving nothing). Bounded: a race that wrongly kept the laptop would
        // dial an unreachable id.
        let e = resolve_peer_endpoint(&mesh, "b64u:mallory")
            .await
            .expect_err("resolving by identity must refuse the laptop");
        assert!(format!("{e:#}").contains("REVOKED"), "{e:#}");
        let raced = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::daemon::dial::dial_service(&mesh, "b64u:mallory", "notes"),
        )
        .await
        .expect("the refusal is immediate, not a dial timeout");
        let e = match raced {
            Ok(_) => panic!("the person race must refuse every device of a revoked identity"),
            Err(e) => e,
        };
        assert!(
            format!("{e:#}").contains("every device of that peer is REVOKED"),
            "{e:#}"
        );

        unrevoke("b64u:mallory").await.unwrap();
        for sel in ["mallory-laptop", "mallory-phone", "b64u:mallory"] {
            resolve_peer_endpoint(&mesh, sel).await.unwrap_or_else(|e| {
                panic!("unrevoking the identity restores the outbound path ({sel}): {e:#}")
            });
        }
    }

    /// #212: `service_allow_grant` to a REVOKED principal is refused with a coded error BEFORE
    /// anything is written — config, ephemeral overlay, or the live registry. It used to succeed
    /// and mint an `allow` entry that admission would never honour.
    ///
    /// Both sides of the boundary are seeded: mallory (revoked) and alice (live) in one store, one
    /// service. Revocation goes through the REAL `peer_revoke`, and the assertions read back the
    /// config file and the live registry — not a flag.
    ///
    /// Deleting the `principal_is_revoked` gate in `grant_service_allow` fails the first
    /// `expect_err`: the grant succeeds and the config gains mallory's `eid:`.
    #[tokio::test(flavor = "multi_thread")]
    async fn granting_a_revoked_principal_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let mallory_eid = mcpmesh_net::EndpointId::from_bytes([3u8; 32]).principal();
        let alice_eid = mcpmesh_net::EndpointId::from_bytes([9u8; 32]).principal();
        for (bytes, nickname, user_id) in [
            ([3u8; 32], "mallory", Some("b64u:mallory")),
            ([9u8; 32], "alice", None),
        ] {
            mesh.store
                .add(PeerEntry {
                    endpoint_id: bytes,
                    nickname: nickname.into(),
                    services: Vec::new(),
                    paired_at: None,
                    user_id: user_id.map(str::to_owned),
                    last_addr: None,
                })
                .unwrap();
        }
        let config_allow = || {
            Config::load(&config_path).unwrap().services["kb"]
                .allow
                .clone()
        };
        let live_allow = || {
            mesh.live_services()
                .get("kb")
                .expect("kb is live")
                .allow
                .clone()
        };
        let revoke = |peer: &str| {
            peer_revoke(
                &state,
                mcpmesh_local_api::PeerRevokeParams {
                    peer: peer.into(),
                    reason: None,
                },
            )
        };

        revoke("mallory").await.unwrap();
        let e = grant_service_allow(&mesh, "kb".into(), mallory_eid.clone())
            .await
            .expect_err("a grant to a revoked eid: must be refused");
        let refusal = e.downcast_ref::<PrincipalRevoked>().unwrap_or_else(|| {
            panic!("the refusal must be the coded PrincipalRevoked, got: {e:#}")
        });
        assert_eq!(refusal.0, mallory_eid, "the refusal names the principal");
        assert!(
            config_allow().is_empty(),
            "nothing written: {:?}",
            config_allow()
        );
        assert!(
            live_allow().is_empty(),
            "registry untouched: {:?}",
            live_allow()
        );

        // The PERSON is not revoked by a device revocation, so their `b64u:` is grantable — the
        // boundary is exactly where admission draws it (another of their devices would be served).
        grant_service_allow(&mesh, "kb".into(), "b64u:mallory".into())
            .await
            .expect("a device revocation does not revoke the identity");
        assert_eq!(config_allow(), vec!["b64u:mallory".to_string()]);

        // Now revoke the person: the identity row lands and every known device is refused, so the
        // `b64u:` is refused too — including an idempotent re-grant of the entry that exists. The
        // verb tells the truth about what the entry is worth rather than answering `{}`.
        revoke("b64u:mallory").await.unwrap();
        let e = grant_service_allow(&mesh, "kb".into(), "b64u:mallory".into())
            .await
            .expect_err("a grant to a revoked identity must be refused");
        assert!(e.downcast_ref::<PrincipalRevoked>().is_some(), "{e:#}");
        assert_eq!(
            config_allow(),
            vec!["b64u:mallory".to_string()],
            "the existing entry is neither duplicated nor stripped"
        );

        // The live side of the boundary is unaffected.
        grant_service_allow(&mesh, "kb".into(), alice_eid.clone())
            .await
            .expect("an unrevoked principal is granted as before");
        assert_eq!(
            config_allow(),
            vec!["b64u:mallory".to_string(), alice_eid.clone()]
        );
        assert!(live_allow().contains(&alice_eid));

        // Lifting only the DEVICE revocation is not enough while the identity row stands: the gate
        // refuses the device on the identity its row carries (#218), so the eid: is still revoked.
        // (This test used to assert the opposite — it certified the #218 defect.)
        peer_unrevoke(
            &state,
            mcpmesh_local_api::PeerUnrevokeParams {
                peer: "mallory".into(),
            },
        )
        .await
        .unwrap();
        let e = grant_service_allow(&mesh, "kb".into(), mallory_eid.clone())
            .await
            .expect_err("a device of a still-revoked identity is not grantable");
        assert!(e.downcast_ref::<PrincipalRevoked>().is_some(), "{e:#}");

        // Lifting the identity makes the eid: grantable again — the refusal was the tables' state,
        // not a permanent property of the principal.
        peer_unrevoke(
            &state,
            mcpmesh_local_api::PeerUnrevokeParams {
                peer: "b64u:mallory".into(),
            },
        )
        .await
        .unwrap();
        grant_service_allow(&mesh, "kb".into(), mallory_eid.clone())
            .await
            .expect("an unrevoked eid: is grantable");
        assert!(config_allow().contains(&mallory_eid));
    }

    /// #218: an introduction whose SUBJECT proves an identity this node has REVOKED is refused
    /// with the coded `PrincipalRevoked` (`-32056` on the wire) before any row is written. It used
    /// to install the row — the person's next device, exactly what `peer_revoke b64u:` meant to
    /// refuse — and the gate then refused every session from it.
    ///
    /// Fixture: mallory is a person we paired with once (her laptop row carries her `user_id`),
    /// revoked through the REAL `peer_revoke`; the phone is a NEW endpoint the endpoint table has
    /// never seen, so the identity row alone must carry the refusal. Carol, a paired endorser,
    /// vouches for the phone and mallory proves the identity with a fresh binding — every check
    /// ahead of the revocation one passes. The inverse is pinned: `peer_unrevoke` on the identity
    /// makes the same introduction succeed, and the gate admits the row it writes.
    ///
    /// Deleting the `is_user_revoked` check in `introduce_peer` fails the first `expect_err`.
    #[tokio::test(flavor = "multi_thread")]
    async fn introducing_a_revoked_identity_is_refused_until_it_is_unrevoked() {
        use mcpmesh_local_api::PeerIntroduceParams;
        use mcpmesh_trust::keys::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        let (carol, _) = UserKey::load_or_generate(&dir.path().join("carol.key")).unwrap();
        let carol_uid = mcpmesh_trust::binding::user_id(&carol);
        let (mallory, _) = UserKey::load_or_generate(&dir.path().join("mallory.key")).unwrap();
        let mallory_uid = mcpmesh_trust::binding::user_id(&mallory);
        let laptop = iroh::SecretKey::from_bytes(&[0xA1; 32]).public();
        for (eid, nickname, uid) in [
            ([0xC0; 32], "carol", &carol_uid),
            (*laptop.as_bytes(), "mallory", &mallory_uid),
        ] {
            mesh.store
                .add(PeerEntry {
                    endpoint_id: eid,
                    nickname: nickname.into(),
                    services: vec![],
                    paired_at: Some("1".into()),
                    user_id: Some(uid.clone()),
                    last_addr: None,
                })
                .unwrap();
        }
        peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: mallory_uid.clone(),
                reason: Some("left the team".into()),
            },
        )
        .await
        .unwrap();
        assert!(
            mesh.store.is_user_revoked(&mallory_uid),
            "fixture: the identity row is written"
        );

        let phone = iroh::SecretKey::from_bytes(&[0xA2; 32]).public();
        let phone_eid = *phone.as_bytes();
        assert!(
            !mesh.store.is_revoked(&phone_eid),
            "fixture: the phone is unknown to the endpoint table — the identity row alone refuses"
        );
        let params = || PeerIntroduceParams {
            subject: phone.to_string(),
            endorsed_by: carol_uid.clone(),
            evidence: mcpmesh_trust::binding::endorse(&carol, &phone_eid, Some(&mallory_uid))
                .unwrap(),
            subject_user_id: Some(mallory_uid.clone()),
            subject_binding: Some(mcpmesh_trust::binding::present(&mallory, &phone_eid).1),
            nickname: "mallory-phone".into(),
        };

        let e = introduce_peer(&state, params())
            .await
            .expect_err("a subject proving a revoked identity must be refused");
        let refusal = e.downcast_ref::<PrincipalRevoked>().unwrap_or_else(|| {
            panic!("the refusal must be the coded PrincipalRevoked, got: {e:#}")
        });
        assert_eq!(refusal.0, mallory_uid, "the refusal names the identity");
        assert!(
            mesh.store.resolve(&phone_eid).unwrap().is_none(),
            "nothing written"
        );

        // The same refusal on the wire, with the code a consumer branches on.
        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "peer_introduce",
            "params": serde_json::to_value(params()).unwrap()
        });
        let r = crate::control::handle_request(&req, &state).await;
        assert_eq!(
            r["error"]["code"],
            mcpmesh_local_api::ERR_PRINCIPAL_REVOKED,
            "peer_introduce must answer -32056 for a revoked identity, got: {r}"
        );
        assert!(
            r["error"]["message"]
                .as_str()
                .unwrap()
                .contains("peer_unrevoke"),
            "the message names the remedy: {r}"
        );
        assert!(mesh.store.resolve(&phone_eid).unwrap().is_none());

        // The other side of the boundary: a subject proving NO identity is not this check's
        // concern, and carol's power to introduce is untouched by mallory's revocation.
        let dave = iroh::SecretKey::from_bytes(&[0xDA; 32]).public();
        introduce_peer(
            &state,
            PeerIntroduceParams {
                subject: dave.to_string(),
                endorsed_by: carol_uid.clone(),
                evidence: mcpmesh_trust::binding::endorse(&carol, dave.as_bytes(), None).unwrap(),
                subject_user_id: None,
                subject_binding: None,
                nickname: "dave".into(),
            },
        )
        .await
        .expect("an identity-less subject is introduced as before");

        // The inverse: lifting the IDENTITY revocation makes the same introduction land, and the
        // gate admits the row it writes.
        peer_unrevoke(
            &state,
            mcpmesh_local_api::PeerUnrevokeParams {
                peer: mallory_uid.clone(),
            },
        )
        .await
        .unwrap();
        introduce_peer(&state, params())
            .await
            .expect("an unrevoked identity is introducible again");
        let row = mesh
            .store
            .resolve(&phone_eid)
            .unwrap()
            .expect("the phone is installed");
        assert_eq!(row.user_id.as_deref(), Some(mallory_uid.as_str()));
        assert!(
            mesh.gate.resolve(&phone_eid.into()).is_some(),
            "and admission agrees"
        );
    }

    /// #218 review: an `eid:` naming a device whose ROW carries a revoked identity is revoked —
    /// by the predicate, on `status`, and on `service_allow_grant` — because the gate refuses that
    /// device. The `eid:` arm used to read only the endpoint table, so in exactly the state a
    /// per-device `peer_unrevoke` leaves (identity row, no endpoint row) `status` showed the grant
    /// and the grant verb accepted a fresh one while every session was refused.
    ///
    /// Fixture: the laptop [5;32] carries `b64u:mallory`, and the identity is revoked with NO
    /// endpoint revocation (asserted); alice [6;32] is the live side of the boundary, granted in
    /// the same service. Inverse: lifting the identity restores all three answers.
    ///
    /// Reverting the `eid:` arm of `principal_is_revoked` to `PeerStore::is_revoked` fails the
    /// first assertion, and the `status` / wire assertions after it.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_eid_of_a_device_carrying_a_revoked_identity_is_revoked_everywhere_admission_says() {
        let laptop = mcpmesh_net::EndpointId::from_bytes([5u8; 32]).principal();
        let alice = mcpmesh_net::EndpointId::from_bytes([6u8; 32]).principal();
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = [\"{laptop}\", \"{alice}\"]\n"
            ),
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        for (b, nickname, uid) in [
            (5u8, "mallory-laptop", "b64u:mallory"),
            (6, "alice", "b64u:alice"),
        ] {
            mesh.store
                .add(PeerEntry {
                    endpoint_id: [b; 32],
                    nickname: nickname.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: Some(uid.into()),
                    last_addr: None,
                })
                .unwrap();
        }
        mesh.store
            .revoke_user(
                "b64u:mallory",
                &crate::allowlist::RevokedEntry {
                    endpoint_id: [0u8; 32],
                    revoked_at: 1,
                    reason: None,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                },
            )
            .unwrap();
        assert!(
            !mesh.store.is_revoked(&[5u8; 32]),
            "fixture: no ENDPOINT revocation — the identity row alone must carry this"
        );
        assert!(
            mesh.gate.resolve(&[5u8; 32].into()).is_none(),
            "fixture: the gate refuses the laptop"
        );
        let kb_allow = || {
            crate::control::status_result(&state)
                .unwrap()
                .services
                .into_iter()
                .find(|s| s.name == "kb")
                .expect("kb service in status")
                .allow
        };
        let grant = |principal: &str, id: u64| {
            let req = serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "service_allow_grant",
                "params": {"service": "kb", "principal": principal}
            });
            let state = &state;
            async move { crate::control::handle_request(&req, state).await }
        };

        assert!(
            principal_is_revoked(&mesh.store, None, &laptop),
            "the eid: of a device the gate refuses on its identity is revoked"
        );
        assert!(!principal_is_revoked(&mesh.store, None, &alice));
        assert_eq!(
            kb_allow(),
            vec![alice.clone()],
            "status hides only the laptop"
        );
        let r = grant(&laptop, 1).await;
        assert_eq!(
            r["error"]["code"],
            mcpmesh_local_api::ERR_PRINCIPAL_REVOKED,
            "a grant to it is refused -32056, got: {r}"
        );
        let r = grant(&alice, 2).await;
        assert!(r.get("error").is_none(), "alice is granted as before: {r}");

        assert!(mesh.store.unrevoke_user("b64u:mallory").unwrap());
        assert!(!principal_is_revoked(&mesh.store, None, &laptop));
        assert_eq!(kb_allow(), vec![laptop.clone(), alice.clone()]);
        let r = grant(&laptop, 3).await;
        assert!(
            r.get("error").is_none(),
            "an unrevoked identity's device is grantable: {r}"
        );
    }

    /// #212: the refusal reaches the wire as `-32056` (`ERR_PRINCIPAL_REVOKED`), not `-32000` —
    /// the code is what a consumer branches on. Also pins that `peer_revoke` and the grant agree
    /// on how a principal is spelled: the revocation is keyed by endpoint BYTES and the grant
    /// arrives as the `eid:` RENDERING, and the two must meet.
    ///
    /// Deleting the `PrincipalRevoked` arm in `respond` fails this: the error falls through to
    /// `-32000`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_grant_to_a_revoked_principal_answers_the_coded_refusal_on_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        // Never paired: a bare eid: may be revoked pre-emptively, and a grant to it is still a
        // grant to a revoked principal.
        let stolen = mcpmesh_net::EndpointId::from_bytes([77u8; 32]).principal();
        peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: stolen.clone(),
                reason: Some("compromised before pairing".into()),
            },
        )
        .await
        .unwrap();

        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "service_allow_grant",
            "params": {"service": "kb", "principal": stolen}
        });
        let r = crate::control::handle_request(&req, &state).await;
        assert_eq!(
            r["error"]["code"],
            mcpmesh_local_api::ERR_PRINCIPAL_REVOKED,
            "service_allow_grant must answer -32056 for a revoked principal, got: {r}"
        );
        let msg = r["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains("is revoked on this node") && msg.contains("peer_unrevoke"),
            "the message must say why and name the remedy: {msg}"
        );
        assert!(
            Config::load(&config_path).unwrap().services["kb"]
                .allow
                .is_empty(),
            "nothing written"
        );

        // The revocation check runs BEFORE the service is resolved, so a revoked principal on an
        // unknown service answers -32056, not -32040. Deliberate: the answer is about the
        // principal, which is the more security-relevant half, and it costs no config read.
        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "service_allow_grant",
            "params": {"service": "ghost", "principal": stolen}
        });
        let r = crate::control::handle_request(&req, &state).await;
        assert_eq!(r["error"]["code"], mcpmesh_local_api::ERR_PRINCIPAL_REVOKED);
    }

    /// #212: the predicate is exactly the gate's question. A roster-revoked `eid:` is refused by
    /// `ComposedGate::resolve` rule 1 even with a clean pairing table, and a bare roster name is
    /// never "revoked" by this table at all.
    #[test]
    fn principal_is_revoked_matches_the_composed_gate_sources() {
        let dir = tempfile::tempdir().unwrap();
        let store = PeerStore::open(&dir.path().join("state.redb")).unwrap();
        let laptop = mcpmesh_net::EndpointId::from_bytes([2u8; 32]);
        // The gate-test roster: alice in `team-eng`, ONE device (the laptop), which the roster
        // itself revokes — the same shape `roster::gate`'s tests use.
        let view = {
            use mcpmesh_trust::roster::sign::mint_signed;
            use mcpmesh_trust::roster::validate::load_installed;
            use mcpmesh_trust::roster::{Roster, RosterDevice, RosterUser, encode_b64u};
            let root = mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
            let r = mint_signed(
                &root,
                Roster {
                    format: "mcpmesh-roster/1".into(),
                    org_id: "acme".into(),
                    serial: 5,
                    issued_at: "2000-01-01T00:00:00Z".into(),
                    expires_at: "2999-01-01T00:00:00Z".into(),
                    groups: vec!["team-eng".into()],
                    users: vec![RosterUser {
                        user_id: "alice".into(),
                        display_name: "Alice".into(),
                        user_pk: encode_b64u(&[1u8; 32]),
                        groups: vec!["team-eng".into()],
                        devices: vec![RosterDevice {
                            endpoint_id: encode_b64u(laptop.as_bytes()),
                            label: "laptop".into(),
                            role: "primary".into(),
                        }],
                    }],
                    revoked_endpoints: vec![encode_b64u(laptop.as_bytes())],
                    successor_root_pk: None,
                    successor_sig: None,
                    sig: String::new(),
                },
            );
            load_installed(&r, &root.verifying_key()).unwrap()
        };
        assert!(
            view.is_revoked(laptop.as_bytes()),
            "fixture: the roster revokes the laptop"
        );
        assert!(
            !store.is_revoked(laptop.as_bytes()),
            "fixture: the pairing table does not"
        );

        assert!(
            principal_is_revoked(&store, Some(&view), &laptop.principal()),
            "a roster-revoked eid: is refused by the composed gate, so it is revoked here"
        );
        assert!(
            !principal_is_revoked(&store, None, &laptop.principal()),
            "…and without that roster it is not"
        );
        assert!(
            !principal_is_revoked(&store, Some(&view), "team-eng"),
            "a bare roster group is never revoked by this table"
        );
        assert!(
            !principal_is_revoked(&store, Some(&view), "alice"),
            "nor a bare roster user_id"
        );
        assert!(
            !principal_is_revoked(&store, Some(&view), "b64u:nobody"),
            "an identity nobody revoked is not revoked"
        );
    }
}

// ============================================================================
// #85 ask 4 — pairing-mode revocation
// ============================================================================

/// Parse an `eid:<64 hex>` principal back to an endpoint id (#85 ask 4).
///
/// The inverse of [`mcpmesh_net::EndpointId::principal`], and deliberately strict: anything that is
/// not exactly that shape returns `None` rather than a best guess. Only `peer_unrevoke` needs it —
/// to lift a revocation on an endpoint whose pair row is gone, which no store lookup can resolve.
fn principal_to_endpoint(principal: &str) -> Option<mcpmesh_net::EndpointId> {
    let hex = principal.strip_prefix("eid:")?;
    let bytes = data_encoding::HEXLOWER.decode(hex.as_bytes()).ok()?;
    let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
    Some(mcpmesh_net::EndpointId::from_bytes(arr))
}

/// `service_allow_grant` (#212) or `peer_introduce` (#218) named a principal this node has
/// REVOKED. A distinct error type so `respond` can map it to
/// [`ERR_PRINCIPAL_REVOKED`](mcpmesh_local_api::ERR_PRINCIPAL_REVOKED) — the same `downcast_ref`
/// idiom [`NoSuchService`] uses.
#[derive(Debug)]
pub struct PrincipalRevoked(pub String);

impl std::fmt::Display for PrincipalRevoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "principal '{}' is revoked on this node, so a row or grant for it would admit nobody — \
             nothing was written. Lift the revocation first (peer_unrevoke) if it was a mistake",
            self.0
        )
    }
}
impl std::error::Error for PrincipalRevoked {}

/// Is `principal` — an `allow`-list entry — one that admission REFUSES on revocation grounds, so a
/// grant to it admits nobody and `status` must not report it as granted (#212)?
///
/// This answers the question the accept path answers, from the same tables, re-read LIVE on every
/// call. It is never wider than admission (nothing it calls revoked is admitted); it can be
/// narrower in one roster-masked corner — a device paired with `user_id = b64u:x` that is ALSO in
/// the installed roster resolves to its ROSTER identity, so a `b64u:x` entry never admits it
/// while the roster is live, yet this predicate reports the entry as honoured. A second, #218's:
/// a ROSTERED device whose roster `user_id` is spelled as a revoked `b64u:` identity is refused by
/// the composed gate's rule 2, and an `eid:` entry naming it still shows — the roster view can be
/// degraded, and then the gate falls through to the pair row instead, so reporting it revoked here
/// would make this predicate WIDER than admission, which is the direction it must never err in.
///
/// - an **`eid:`** is revoked iff [`PeerStore::is_refused`] refuses its endpoint — the endpoint
///   revocation table, or (#218) the identity its stored row carries — or the installed roster's
///   `revoked_endpoints`: the sources `ComposedGate::resolve` consults first, before any service
///   authz runs. Before this, only the endpoint table was read, so after `peer_revoke b64u:x` and a
///   per-device `peer_unrevoke`, the gate refused the device while `status` showed its `eid:` as
///   granted and `service_allow_grant` accepted it. The entry is PARSED
///   with the strict inverse of `EndpointId::principal` rather than compared as a string, so the
///   lookup is the same byte-keyed read the gate performs; a string that is not that exact
///   rendering admits nobody in the first place and is not this predicate's concern.
/// - a **`b64u:`** identity is revoked iff `peer_revoke` revoked the PERSON
///   ([`PeerStore::is_user_revoked`]). Since #218 that is the whole answer: the gate refuses every
///   row carrying a revoked `user_id` ([`PeerStore::admission`], consulted at every entry
///   point), and redemption and `peer_introduce` refuse to write one. #212 first shipped this
///   arm with a second clause — "AND none of its known devices is admitted" — because the gate of
///   the day admitted a per-device-unrevoked row through its pair row; that gate was the #218
///   defect, and the clause described it rather than the intent.
/// - a **bare** entry (a roster group, a roster `user_id`, a legacy nickname) is never revoked here:
///   roster membership is withdrawn by roster INSTALL, which rebuilds the view, not by this table.
///
/// Fails CLOSED like the reads it composes: an unreadable store answers "revoked", which is also
/// what the gate would answer.
pub(crate) fn principal_is_revoked(
    store: &PeerStore,
    roster: Option<&mcpmesh_trust::roster::validate::RosterView>,
    principal: &str,
) -> bool {
    if let Some(id) = principal_to_endpoint(principal) {
        return store.is_refused(id.as_bytes())
            || roster.is_some_and(|v| v.is_revoked(id.as_bytes()));
    }
    principal.starts_with("b64u:") && store.is_user_revoked(principal)
}

/// The `mcpmesh-revoke:` token scheme, mirroring `mcpmesh-invite:` / `mcpmesh-enroll:`.
const REVOKE_SCHEME: &str = "mcpmesh-revoke:";

/// The wire body of a signed device revocation (#85 ask 4).
///
/// Every field is inside the signature except `sig` itself — see
/// [`mcpmesh_trust::roster::sign::sign_device_revocation`]. `reason` is deliberately OUTSIDE it:
/// it is an operator note for humans, and signing it would mean a typo could not be corrected
/// without re-issuing from a device that may no longer exist.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RevocationToken {
    /// The signer's `b64u:` user_id.
    user_id: String,
    /// The revoked endpoint's `eid:` principal.
    endpoint: String,
    /// Epoch seconds, INSIDE the signature — orders two statements about one endpoint and makes a
    /// replayed older token a no-op.
    issued_at: u64,
    /// base64url of the 64-byte ed25519 signature.
    sig: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Resolve a revocation target (nickname | `eid:` | `b64u:`) to endpoint ids.
///
/// Nicknames are accepted here even though `allow` lists deliberately never hold them (#38): this
/// is not an authorization grant, it is the operator naming a device they want dead, and the
/// nickname is what they know it by. `peer_remove` takes one for the same reason.
///
/// **The vocabulary is wider than the docs used to say**, because this delegates to
/// `endpoints_for_principal`, which in roster mode also matches a roster `user_id` and a roster
/// GROUP NAME. So `revoke peer engineering` revokes every device in that group. That is a coherent
/// operation and the sever path already accepts the same vocabulary — but it was undocumented,
/// which made it a surprise rather than a feature. Named in `docs/local-protocol.md` now.
async fn revocation_targets(
    mesh: &Arc<MeshState>,
    peer: &str,
) -> Result<Vec<mcpmesh_net::EndpointId>> {
    let store = mesh.store.clone();
    let roster = mesh.roster.view();
    let want = peer.to_string();
    blocking("join revocation target resolution", move || {
        let mut out: std::collections::HashSet<mcpmesh_net::EndpointId> =
            crate::daemon::sever::endpoints_for_principal(&store, roster.as_deref(), &want)?;
        // …plus the nickname arm, which `endpoints_for_principal` does not cover.
        for e in store.list()? {
            if e.nickname == want {
                out.insert(mcpmesh_net::EndpointId::from_bytes(e.endpoint_id));
            }
        }
        anyhow::Ok(out.into_iter().collect::<Vec<_>>())
    })
    .await?
}

/// `peer_revoke` (#85 ask 4): mark an endpoint dead LOCALLY, and cut it off now.
pub async fn peer_revoke(
    state: &DaemonState,
    params: mcpmesh_local_api::PeerRevokeParams,
) -> Result<mcpmesh_local_api::PeerRevokeResult> {
    let mesh = state.mesh_required()?;
    let mut targets = revocation_targets(mesh, &params.peer).await?;
    // A bare, well-formed `eid:` is accepted even with NO matching row.
    //
    // An operator told "this endpoint id is compromised" must be able to act before they have ever
    // paired with it — otherwise the only way to pre-empt a bad device is to pair with it first,
    // which is absurd. Unlike the SIGNED import, this carries no denial-of-service risk: it is
    // local-only and the operator made the decision themselves.
    if targets.is_empty()
        && let Some(id) = principal_to_endpoint(&params.peer)
    {
        targets.push(id);
    }
    // Anything else that resolves to nothing is an ERROR, never an empty success. This is a
    // revocation surface: a typo reading as "done" would leave an operator believing they had cut
    // off a device they had not touched — the same reasoning `peer_remove` uses for its all-no-op
    // case. An `eid:` is exempt above precisely because it is unambiguous; a mistyped nickname or
    // `b64u:` is not.
    anyhow::ensure!(
        !targets.is_empty(),
        "no paired device matches {:?} (expected a nickname, a b64u: user_id, or a well-formed \
         eid: principal — an eid: may be revoked before you have paired with it)",
        params.peer
    );

    let store = mesh.store.clone();
    let reason = params.reason.clone();
    let ids: Vec<[u8; 32]> = targets.iter().map(|e| *e.as_bytes()).collect();
    let now = crate::util::epoch_now_u64();
    // A `b64u:` revokes the IDENTITY as well as its known endpoints (#85 ask 3 gate).
    //
    // Per-endpoint revocation cannot express "not on any of their machines" once attestation
    // exists: a device holding that person's user key can mint a fresh endpoint id at will, so a
    // list of the ids we happen to know is a list of the ones the attacker will not reuse. The CLI
    // has always described this verb as revoking the person; now it does.
    let revoked_user = params
        .peer
        .starts_with("b64u:")
        .then(|| params.peer.clone());
    if let Some(uid) = revoked_user.clone() {
        let store = mesh.store.clone();
        let reason = params.reason.clone();
        blocking("join identity revoke write", move || {
            store.revoke_user(
                &uid,
                &crate::allowlist::RevokedEntry {
                    endpoint_id: [0u8; 32],
                    revoked_at: now,
                    reason,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                },
            )
        })
        .await??;
    }
    blocking("join peer revoke write", move || {
        for id in ids {
            // Do NOT overwrite a SIGNED revocation with a local one (#85 ask 4, 0.45.0 gate).
            //
            // Two things break if we do. The provenance is destroyed — `status` stops showing that
            // the device's OWNER declared it dead, which is strictly stronger evidence than the
            // operator's own note. And clearing `issued_at` disarms the replay guard: with no prior
            // timestamp, an OLDER token from that peer re-applies and wins.
            //
            // The endpoint is already revoked either way, so there is nothing to gain by
            // rewriting the row — only provenance to lose.
            if let Some(existing) = store.revoked_entry(&id)?
                && existing.source == "signed"
            {
                continue;
            }
            store.revoke(crate::allowlist::RevokedEntry {
                endpoint_id: id,
                revoked_at: now,
                reason: reason.clone(),
                source: "local".into(),
                signer_user_id: None,
                issued_at: None,
            })?;
        }
        anyhow::Ok(())
    })
    .await??;

    // IMMEDIATE, like `service_allow_revoke` (#54). The write above already makes the gate refuse
    // NEW connections and makes `should_sever_now` true for the check-register recheck; this cuts
    // the ones already established, which would otherwise run for as long as the peer kept them —
    // unbounded for a warm MCP session.
    let principals: Vec<String> = targets.iter().map(|e| e.principal()).collect();
    // A `b64u:` is ALSO severed as itself (#218), re-enumerated here, AFTER the writes. `targets`
    // was computed before them, so a device that paired with this identity in between (a
    // redemption or introduction landing mid-revoke) would hold a live session nobody cuts. The
    // gate refuses it from the identity row onward, and `sever_principals` resolves the identity to
    // every row carrying it now.
    let mut sever: Vec<String> = principals.clone();
    sever.extend(revoked_user.clone());
    let severed = sever_principals(mesh, &sever).await?;

    for p in &principals {
        mesh.audit().record(AuditRecord::trust(
            now_ts(),
            "peer_revoke".into(),
            Some(params.peer.clone()),
            Some(p.clone()),
        ));
    }
    tracing::warn!(peer = %params.peer, count = principals.len(), severed, "peer revoked");
    Ok(mcpmesh_local_api::PeerRevokeResult {
        revoked: principals,
        severed,
    })
}

/// `peer_unrevoke` (#85 ask 4). Idempotent: lifting a revocation nobody holds is a clean empty
/// result, unlike `peer_revoke`'s unmatched name — undoing nothing is harmless, cutting off nothing
/// while reporting success is not.
pub async fn peer_unrevoke(
    state: &DaemonState,
    params: mcpmesh_local_api::PeerUnrevokeParams,
) -> Result<mcpmesh_local_api::PeerUnrevokeResult> {
    let mesh = state.mesh_required()?;
    let mut targets = revocation_targets(mesh, &params.peer).await?;
    // A revoked endpoint whose pair row was ALSO removed resolves to nothing above, so accept a
    // bare `eid:` directly — otherwise a revocation could become permanently unliftable, which is
    // exactly the state an operator hits after revoking and then unpairing.
    if targets.is_empty()
        && let Some(id) = principal_to_endpoint(&params.peer)
    {
        targets.push(id);
    }
    let store = mesh.store.clone();
    let ids: Vec<[u8; 32]> = targets.iter().map(|e| *e.as_bytes()).collect();
    if params.peer.starts_with("b64u:") {
        let store = mesh.store.clone();
        let uid = params.peer.clone();
        blocking("join identity unrevoke write", move || {
            store.unrevoke_user(&uid)
        })
        .await??;
    }
    let lifted = blocking("join peer unrevoke write", move || {
        let mut lifted = Vec::new();
        for id in ids {
            if store.unrevoke(&id)? {
                lifted.push(mcpmesh_net::EndpointId::from_bytes(id).principal());
            }
        }
        anyhow::Ok(lifted)
    })
    .await??;

    for p in &lifted {
        mesh.audit().record(AuditRecord::trust(
            now_ts(),
            "peer_unrevoke".into(),
            Some(params.peer.clone()),
            Some(p.clone()),
        ));
    }
    tracing::info!(peer = %params.peer, count = lifted.len(), "revocation lifted");
    Ok(mcpmesh_local_api::PeerUnrevokeResult { unrevoked: lifted })
}

/// Read this node's user key, or explain why it has none.
///
/// READ, never `load_or_generate` — the same discipline `user_key_export` follows: minting a key
/// here would let a node with no identity sign a revocation under a freshly-invented one, which no
/// peer would recognise and which would then become this node's identity at the next restart.
pub(crate) async fn read_user_key(mesh: &Arc<MeshState>) -> Result<mcpmesh_trust::UserKey> {
    let path = mesh
        .user_key_path
        .get()
        .cloned()
        .context("this node has no user key path resolved")?;
    anyhow::ensure!(
        path.exists(),
        "this node has no user key, so it cannot speak for a person ({}). \
         Pair with a binding, join an org, or import a recovery phrase first.",
        path.display()
    );
    let bytes = blocking("join user key read", {
        let path = path.clone();
        move || std::fs::read(&path)
    })
    .await?
    .with_context(|| format!("read user key at {}", path.display()))?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("the user key at {} is not 32 bytes", path.display()))?;
    Ok(mcpmesh_trust::UserKey::from_signing_key(
        mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&bytes),
    ))
}

/// `device_revoke` (#85 ask 4): sign a portable revocation of one of MY OWN devices.
///
/// The direction local revocation cannot express. My peers cannot discover that my laptop was
/// stolen; I have to tell them, and they have to be able to verify it was me.
pub(crate) async fn device_revoke(
    state: &DaemonState,
    params: mcpmesh_local_api::DeviceRevokeParams,
) -> Result<mcpmesh_local_api::DeviceRevokeResult> {
    let mesh = state.mesh_required()?;
    let endpoint = principal_to_endpoint(&params.endpoint).with_context(|| {
        format!(
            "{:?} is not an eid: device principal (expected eid:<64 hex>)",
            params.endpoint
        )
    })?;
    // #86 gate, the one `peer_endorse` has and this verb lacked (#214): an ENROLLED device holds no
    // authority over the identity it presents. `read_user_key` below reads the LOCAL key boot mints
    // regardless, so without this the call self-applied the revocation, severed sessions, audited
    // it and returned a token signed under a `b64u:` no peer has ever paired with — a silent
    // partial success on the stolen-laptop path. Refused BEFORE any of those side effects.
    //
    // #221: checked under `user_key_lock` and held through the key read and the signature, so an
    // adoption cannot land between the check and the token. Released before the self-apply and
    // the session sever: the token is already signed by a device that held the key.
    let key_guard = lock_own_user_key(mesh, "device_revoke", "Revoke").await?;
    mesh.run_identity_gate_hook().await;
    let key = read_user_key(mesh).await?;
    let user_id = mcpmesh_trust::binding::user_id(&key);
    let issued_at = crate::util::epoch_now_u64();
    let sig = mcpmesh_trust::roster::sign::sign_device_revocation(
        key.signing_key(),
        endpoint.as_bytes(),
        issued_at,
    );
    let token = RevocationToken {
        user_id: user_id.clone(),
        endpoint: endpoint.principal(),
        issued_at,
        sig: mcpmesh_trust::roster::encode_b64u(&sig),
        reason: params.reason.clone(),
    };
    let encoded = format!(
        "{REVOKE_SCHEME}{}",
        data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&token)?)
    );
    drop(key_guard);

    // Apply it to OURSELVES too. A node that hands out a revocation of a device while still
    // admitting that device is telling its peers to do something it does not do — and the stolen
    // laptop may well still be paired here.
    let store = mesh.store.clone();
    let eid_bytes = *endpoint.as_bytes();
    let reason = params.reason.clone();
    blocking("join device revoke self-apply", move || {
        store.revoke(crate::allowlist::RevokedEntry {
            endpoint_id: eid_bytes,
            revoked_at: issued_at,
            reason,
            source: "signed".into(),
            signer_user_id: Some(user_id.clone()),
            issued_at: Some(issued_at),
        })
    })
    .await??;
    let severed = sever_principals(mesh, &[endpoint.principal()]).await?;

    // AUDITED — the event and the subject. The token itself is not a secret (it authorizes
    // nothing), but the record is what lets an operator reconstruct when they declared a device
    // dead, which is the question asked after an incident.
    mesh.audit().record(AuditRecord::trust(
        now_ts(),
        "device_revoke".into(),
        None,
        Some(endpoint.principal()),
    ));
    tracing::warn!(endpoint = %endpoint.principal(), severed, "signed a device revocation");
    Ok(mcpmesh_local_api::DeviceRevokeResult {
        token: encoded,
        endpoint: endpoint.principal(),
        user_id: mcpmesh_trust::binding::user_id(&key),
    })
}

/// `device_revocation_import` (#85 ask 4): apply a peer's signed revocation of their own device.
///
/// Two independent checks, and BOTH are load-bearing:
///
/// 1. **The signature verifies against the claimed `user_id`.** Otherwise anyone could author one.
/// 2. **We already trust that `user_id`, AND the revoked endpoint is one we associate with that
///    same person** (or is entirely unknown to us). Without (2) a paired peer could mark a THIRD
///    party's device dead in our node — a very different and much worse power than "kill your own
///    devices here", which is the only thing this verb is meant to grant. A signature proves who
///    asked; it does not prove the endpoint was ever theirs.
///
/// **An endpoint we have never seen is REFUSED, not recorded.** The first cut accepted it, so a
/// stolen device could be pre-revoked before a peer had met it. The gate proved that is a
/// denial-of-service primitive against everyone you have NOT met — endpoint ids are public, so any
/// paired peer could kill an arbitrary future contact permanently, and the operator's only remedy
/// is to `revoke undo` a principal they never revoked. There is no way to verify ownership of an
/// endpoint we hold no row for; a binding carried in the token would not help, since the signer
/// could bind any endpoint to themselves.
///
/// The cost is real and is stated rather than hidden: a peer who pairs with the stolen device AFTER
/// receiving the token has to import it again once the row exists. Promoting a held "pending"
/// revocation at pairing time would close that without reopening the hole, and is follow-up work.
pub(crate) async fn device_revocation_import(
    state: &DaemonState,
    params: mcpmesh_local_api::DeviceRevocationImportParams,
) -> Result<mcpmesh_local_api::DeviceRevocationImportResult> {
    let mesh = state.mesh_required()?;
    let body = params
        .token
        .trim()
        .strip_prefix(REVOKE_SCHEME)
        .context("not a revocation token (expected a mcpmesh-revoke: line)")?;
    let raw = data_encoding::BASE64URL_NOPAD
        .decode(body.as_bytes())
        .context("revocation token is not valid base64url")?;
    let token: RevocationToken =
        serde_json::from_slice(&raw).context("revocation token is malformed")?;

    let endpoint = principal_to_endpoint(&token.endpoint)
        .context("revocation token names no valid eid: principal")?;
    let user_pk: [u8; 32] = mcpmesh_trust::roster::decode_b64u(&token.user_id)
        .ok()
        .and_then(|v| v.as_slice().try_into().ok())
        .context("revocation token carries no valid b64u: user_id")?;
    let sig = mcpmesh_trust::roster::decode_b64u(&token.sig)
        .context("revocation token signature is not base64url")?;

    // (1) The signature. Before ANY store read, so a forged token costs nothing to refuse.
    mcpmesh_trust::roster::sign::verify_device_revocation(
        &user_pk,
        endpoint.as_bytes(),
        token.issued_at,
        &sig,
    )
    .map_err(|_| anyhow::anyhow!("revocation signature does not verify against its user_id"))?;

    // (2) Authorization: do we trust this signer, and is this endpoint theirs?
    let store = mesh.store.clone();
    let signer = token.user_id.clone();
    let eid_bytes = *endpoint.as_bytes();
    let (trusted, endpoint_is_theirs, endpoint_known, existing_issued_at) =
        blocking("join revocation import checks", move || {
            let entries = store.list()?;
            let trusted = entries
                .iter()
                .any(|e| e.user_id.as_deref() == Some(&signer));
            let row = entries.iter().find(|e| e.endpoint_id == eid_bytes);
            let endpoint_is_theirs = row.is_some_and(|e| e.user_id.as_deref() == Some(&signer));
            let prior = store.revoked_entry(&eid_bytes)?.and_then(|e| e.issued_at);
            anyhow::Ok((trusted, endpoint_is_theirs, row.is_some(), prior))
        })
        .await??;

    anyhow::ensure!(
        trusted,
        "this node does not know {} — a revocation is only honoured from a person you have paired \
         with, or it becomes an unauthenticated way to mark any endpoint dead",
        token.user_id
    );
    // The endpoint MUST already be known to us as that person's device.
    //
    // The first version also accepted an endpoint we had never seen, so a stolen device could be
    // pre-revoked before a peer had met it. The 0.45.0 gate proved the cost, and it is not a close
    // call: endpoint ids are public (invite lines, `status`, dial hints), so any paired peer could
    // sign a revocation of an endpoint we had not met yet and kill it PERMANENTLY on arrival — the
    // pairing would complete and the gate would refuse it, with the only remedy an operator
    // running `revoke undo` on a principal they never revoked. That is the denial-of-service
    // primitive this check exists to prevent, aimed at the larger set: everyone we have NOT met.
    //
    // We cannot verify ownership of an endpoint we hold no row for. A binding carried in the token
    // would not help — the signer could bind any endpoint to themselves, which is the same lie
    // wearing a signature. So: refuse, and say what to do about it.
    anyhow::ensure!(
        endpoint_known,
        "this node has no record of {} — a revocation can only be applied to a device we already \
         know is {}'s. Pair with them first, then import this token again",
        token.endpoint,
        token.user_id
    );
    anyhow::ensure!(
        endpoint_is_theirs,
        "{} is not a device of {} on this node — a peer may revoke their OWN devices here, never \
         a third party's",
        token.endpoint,
        token.user_id
    );

    // Idempotent, and monotonic: an OLDER token must never displace a newer statement. `issued_at`
    // is inside the signature, so it cannot be re-dated to win this comparison.
    if existing_issued_at.is_some_and(|prior| prior >= token.issued_at) {
        return Ok(mcpmesh_local_api::DeviceRevocationImportResult {
            endpoint: endpoint.principal(),
            user_id: token.user_id,
            applied: false,
            severed: 0,
        });
    }

    let store = mesh.store.clone();
    let signer = token.user_id.clone();
    let reason = token.reason.clone();
    let issued_at = token.issued_at;
    let now = crate::util::epoch_now_u64();
    blocking("join revocation import write", move || {
        store.revoke(crate::allowlist::RevokedEntry {
            endpoint_id: eid_bytes,
            revoked_at: now,
            reason,
            source: "signed".into(),
            signer_user_id: Some(signer),
            issued_at: Some(issued_at),
        })
    })
    .await??;
    let severed = sever_principals(mesh, &[endpoint.principal()]).await?;

    mesh.audit().record(AuditRecord::trust(
        now_ts(),
        "device_revocation_import".into(),
        Some(token.user_id.clone()),
        Some(endpoint.principal()),
    ));
    tracing::warn!(
        endpoint = %endpoint.principal(),
        signer = %token.user_id,
        severed,
        "applied a signed device revocation"
    );
    Ok(mcpmesh_local_api::DeviceRevocationImportResult {
        endpoint: endpoint.principal(),
        user_id: token.user_id,
        applied: true,
        severed,
    })
}

/// `attest_offer` (#85 ask 3): mint the `mcpmesh-attest:` line another device of this person dials.
///
/// Refused unless this node actually admits attested devices. An offer we would not honour is worse
/// than no offer: the other device would run a ceremony, be refused generically, and have nothing
/// to tell the operator about why.
pub(crate) async fn attest_offer(
    state: &DaemonState,
) -> Result<mcpmesh_local_api::AttestOfferResult> {
    let mesh = state.mesh_required()?;
    anyhow::ensure!(
        mesh.admit_attested_devices(),
        "this node does not admit attested devices — set `[identity].admit_attested_devices = true` \
         and restart, then offer again"
    );
    let offer = crate::pairing::rendezvous::AttestOffer {
        node_id: *mesh.endpoint.id().as_bytes(),
        node_addr_json: serde_json::to_string(&mesh.endpoint.addr())?,
    };
    Ok(mcpmesh_local_api::AttestOfferResult {
        offer: offer.encode()?,
    })
}

/// `attest_to` (#85 ask 3): present this device's identity to a peer that already pairs with this
/// person, using their offer line.
pub(crate) async fn attest_to(
    state: &DaemonState,
    params: mcpmesh_local_api::AttestToParams,
) -> Result<mcpmesh_local_api::PairResult> {
    let mesh = state.mesh_required()?;
    crate::pairing::rendezvous::attest_to(
        mesh.endpoint.clone(),
        params.offer,
        mesh.store.clone(),
        mesh.self_binding(),
        None,
    )
    .await
}
