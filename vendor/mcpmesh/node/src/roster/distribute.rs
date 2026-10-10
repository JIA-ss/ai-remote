//! Roster DISTRIBUTION: the two automatic convergence channels — the gossip receive loop and the
//! HTTPS URL poll. On a higher-serial announcement (or served body) each fetches the roster bytes
//! and funnels them through the host's SINGLE install pipeline
//! ([`DistributionHost::install_roster_bytes`]: validate → persist → hot-swap → sever, serialized
//! with the manual install). Every accepting node re-seeds its blob + re-announces, so propagation
//! does not depend on the operator staying online. None of the channels validates independently.
//!
//! **The trust boundary is the org-root signature — nothing else.** A gossip announce, the blob it
//! points at, and the `roster_hash` are content-addressed conveniences that only TRIGGER a fetch;
//! they are NOT trust inputs. A stale/equal serial, a hash mismatch, or a validation failure is
//! logged and IGNORED (fail-safe). There is exactly ONE validator, inside the install pipeline,
//! which re-checks `serial > installed` under the host's single-writer lock so a racing URL poll /
//! concurrent announce cannot double-install.
//!
//! This module never sees the daemon's state: everything it needs from its host daemon comes
//! through the narrow [`DistributionHost`] seam the daemon implements.
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh_blobs::ticket::BlobTicket;
use iroh_gossip::api::{GossipReceiver, GossipSender};

use crate::roster::gate::RosterGate;
use crate::roster::transport::{self, RosterAddrBook, RosterAnnounce, RosterBlobs};
use std::time::{Duration, Instant};

/// What the distribution channels need from the daemon that hosts them — the narrow seam that
/// keeps this module (and its loops) free of daemon state. The daemon's mesh state implements it;
/// a test host could implement it over fakes. Async methods declare `+ Send` futures because the
/// loops run the host inside spawned tasks.
pub trait DistributionHost: Send + Sync + 'static {
    /// The endpoint blob publishes/fetches ride on (shared with the whole daemon).
    fn endpoint(&self) -> &iroh::Endpoint;
    /// The live roster gate — read for the installed view/serial (hot-swapped by installs).
    fn roster(&self) -> &RosterGate;
    /// The roster-blob transport, or `None` in a pure-pairing daemon (nothing to distribute).
    fn blobs(&self) -> Option<&RosterBlobs>;
    /// Whether gossip is running (roster mode). `false` → announcing is a no-op.
    fn gossip_active(&self) -> bool;
    /// The path of THIS node's installed `roster.json` (per-node in multi-daemon tests).
    fn installed_roster_path(&self) -> PathBuf;
    /// The pinned org-root pubkey from config, or `None` when none is pinned. The equal-serial
    /// currency confirm re-verifies served bodies against it.
    fn pinned_org_root_pk(&self) -> Result<Option<String>>;
    /// The bounded provider address book, or `None` (tests fall back to a per-fetch lookup).
    fn addr_book(&self) -> Option<Arc<RosterAddrBook>>;
    /// A clone of the roster-topic gossip sender, or `None` (pure-pairing daemon).
    fn roster_topic_sender(&self) -> impl Future<Output = Option<GossipSender>> + Send;
    /// Move the roster-topic gossip receiver out — exactly once, for the receive loop.
    fn take_roster_topic_receiver(&self) -> impl Future<Output = Option<GossipReceiver>> + Send;
    /// Record that the installed roster was CONFIRMED current at `now` (the freshness bump).
    fn confirm_roster_current(&self, now: i64) -> impl Future<Output = ()> + Send;
    /// Converge fetched roster BYTES through the host's SINGLE install pipeline: serialized with
    /// the manual install (single-writer), `serial > installed` re-checked inside the lock, the
    /// org-root signature the sole trust input, then persist → gate hot-swap → sever. Returns
    /// whether an install actually happened (`false` = lost the under-lock serial race — a
    /// fail-safe no-op). The CALLER re-seeds/re-announces on `true`.
    fn install_roster_bytes(
        &self,
        bytes: &[u8],
        serial: u64,
        channel: &'static str,
    ) -> impl Future<Output = Result<bool>> + Send;
    /// Would dialling `endpoint_id` reach a device this host has REVOKED (#223)? Asked before an
    /// announce's blob provider is contacted. BLOCKING (it may read storage): callers run it on the
    /// blocking pool. Required rather than defaulted, because a default would have to pick an
    /// answer, and `false` is fail-open.
    fn dial_refused(&self, endpoint_id: &[u8; 32]) -> bool;
}

/// Per-fetch timeout: a hung/stalling blob provider can't hold a fetch slot forever;
/// on timeout the announce is dropped fail-safe (a re-announce / the URL poll re-converges).
const GOSSIP_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Bounded off-loop fetch concurrency: the single-consumer receive loop SPAWNS each fetch+install
/// holding one of these permits, so a stalling provider never wedges gossip convergence. A full pool
/// DROPS the announce (fail-safe). Bounds concurrent in-flight fetches (no unbounded spawn).
const GOSSIP_FETCH_CONCURRENCY: usize = 4;
/// Announce-processing rate: bounds how often the loop TRIGGERS a fetch, so an announce-spam flood
/// cannot drive unbounded fetches. A single-consumer bucket on the loop (no lock needed).
const GOSSIP_ANNOUNCE_PER_MIN: u32 = 60;
/// The pinned roster-poll timeout: a hung host must not wedge the poll.
const POLL_TIMEOUT: Duration = Duration::from_secs(10);
/// The roster-body size cap: a signed roster for a 20-peer org is a few KiB; 4 MiB is
/// generous. Bounds memory from an oversized/compromised host (the URL is operator-pinned → lower
/// risk, but bounded regardless — no OOM).
const MAX_ROSTER_BYTES: usize = 4 * 1024 * 1024;

/// Announce the CURRENTLY-installed roster on the roster topic: add its bytes to the
/// local blob store (so this node serves it onward), then broadcast `{serial, roster_hash,
/// blob_ticket}`. Called (a) by the operator's publish path after a serial bump (org
/// approve/revoke, wired in the manual install), and (b) by a node that just ACCEPTED a gossip
/// roster (re-seed + re-announce). A pure-pairing daemon (no gossip/blobs) no-ops.
pub async fn announce_roster<H: DistributionHost>(mesh: &Arc<H>) -> Result<()> {
    if !mesh.gossip_active() {
        return Ok(()); // pure-pairing daemon: nothing to announce
    }
    let Some(blobs) = mesh.blobs() else {
        return Ok(()); // pure-pairing daemon: nothing to announce
    };
    let Some(view) = mesh.roster().view() else {
        return Ok(()); // no roster installed yet (a joiner awaiting its first approval)
    };
    let serial = view.serial();
    let path = mesh.installed_roster_path();
    let bytes = crate::util::blocking("join roster read", move || std::fs::read(path))
        .await?
        .context("read installed roster for announce")?;
    let (ticket, roster_hash) = blobs.publish(&bytes, mesh.endpoint()).await?;
    let announce = RosterAnnounce {
        serial,
        roster_hash,
        blob_ticket: ticket,
    };
    if let Some(sender) = mesh.roster_topic_sender().await {
        transport::broadcast(&sender, announce.to_bytes()).await?;
    }
    Ok(())
}

/// Handle ONE received roster announcement: if `serial > installed`, fetch the blob, verify its
/// hash, and CONVERGE through the host's single install pipeline; then re-seed + re-announce.
/// The install is serialized with the manual path inside [`DistributionHost::install_roster_bytes`]
/// (two channels must not race installs). A stale/equal serial, a hash mismatch, or a validation
/// failure is logged + ignored (fail-safe — the org-root signature is the trust boundary).
pub async fn on_announce<H: DistributionHost>(
    mesh: &Arc<H>,
    announce: RosterAnnounce,
) -> Result<()> {
    if announce.serial <= mesh.roster().view().map(|v| v.serial()).unwrap_or(0) {
        return Ok(()); // not newer (also the re-announce idempotence) — fail-safe ignore
    }
    let Some(blobs) = mesh.blobs() else {
        return Ok(()); // pure-pairing daemon (defensive — this loop only spawns in roster mode)
    };
    // Seed the ticket's provider addr into THIS endpoint's address book BEFORE the fetch:
    // `blobs.fetch` resolves the provider by `ticket.addr().id` via the endpoint's address
    // lookup, and the provider (the operator, or a re-seeder) may be a node whose addr we do not
    // already know. The ticket embeds the full `EndpointAddr`, so a per-fetch `MemoryLookup` add
    // makes the fetch resolve. A malformed ticket string falls through to `fetch`, which returns
    // a typed Err (fail-safe). This add is idempotent + additive (never removes a known addr),
    // so it is safe even when the addr is already known (the localhost test case). BOUNDED: the
    // shared RosterAddrBook dedups by id under a cap.
    //
    // FILTERED (#203): the ticket is a remote party's claim, noted before its roster signature is
    // ever checked, so `note` runs `dialable_only` on it first.
    //
    // **That filter is far from sufficient, and the limit is worth stating here.** Nothing binds
    // the ticket's `id` to a roster member, and `blobs.fetch` resolves the provider BY ID — which
    // triggers iroh's address lookup. On the default `presets::N0` endpoint that means pkarr and
    // DNS, and a pkarr record is self-signed by the endpoint key, so an announcer can publish any
    // address list under an id they generated and have it reach the path set with no hygiene at
    // all. The filter here therefore helps mainly where the ticket is the SOLE source — a
    // discovery-less deployment. Bounding the rest means constraining address lookup itself, which
    // is #203's, not this function's.
    if let Ok(ticket) = announce.blob_ticket.parse::<BlobTicket>() {
        // #223: the announce is UNSIGNED gossip, so its provider can be any member — including one
        // this node revoked. The roster signature protects what we would fetch, not the dial:
        // contacting the provider tells it this node's id, address and that it is online, the same
        // leak the reachability probe had. Checked before the address is even noted. Logged
        // without the endpoint id. Fails CLOSED on a join error.
        let (m, provider) = (mesh.clone(), *ticket.addr().id.as_bytes());
        let refused = tokio::task::spawn_blocking(move || m.dial_refused(&provider))
            .await
            .unwrap_or(true);
        if refused {
            tracing::info!(
                serial = announce.serial,
                "roster announce names a blob provider REVOKED on this node; not fetching"
            );
            return Ok(()); // fail-safe: another member's announce or the URL poll converges
        }
        if let Some(book) = mesh.addr_book() {
            // `note` filters (#203) — see `RosterAddrBook::note`. Boot always registers a book, so
            // this is the production path.
            book.note(ticket.addr().clone());
        } else {
            // DEFENSIVE AND CURRENTLY UNREACHABLE: `MeshState` is the only `DistributionHost`, and
            // boot registers the book immediately before spawning this loop, so nothing — not even
            // a fixture — takes this branch. The retained comment above claiming it "covers tests
            // without a registered book" was false. Kept rather than deleted because the trait is
            // public and a future host could omit the book; filtered explicitly for the same
            // reason, since it bypasses `note`. Nothing pins it, and nothing can while it is
            // unreachable.
            let mem = iroh::address_lookup::MemoryLookup::new();
            mem.add_endpoint_info(crate::daemon::dial::dialable_only(ticket.addr().clone()));
            if let Ok(lookup) = mesh.endpoint().address_lookup() {
                lookup.add(mem);
            }
        }
    }
    let bytes = match tokio::time::timeout(
        GOSSIP_FETCH_TIMEOUT,
        blobs.fetch(
            &announce.blob_ticket,
            &announce.roster_hash,
            mesh.endpoint(),
        ),
    )
    .await
    {
        Ok(r) => r.context("fetch announced roster blob")?,
        Err(_) => {
            tracing::debug!("gossip roster fetch timed out; dropping (will re-converge)");
            return Ok(()); // FAIL-SAFE: a re-announce or the URL poll re-converges
        }
    };

    if mesh
        .install_roster_bytes(&bytes, announce.serial, "gossip")
        .await?
    {
        // Re-seed + re-announce so propagation does not depend on the operator staying online.
        return announce_roster(mesh).await;
    }
    Ok(())
}

/// GET `url` with a total `timeout` and a hard body `max`. FAIL-SAFE: a slow host errors (the
/// caller retries next interval, failing toward degraded); an oversized body errors BEFORE the
/// whole thing is buffered (no OOM). Uses `Response::chunk()` under
/// `reqwest default-features=false, rustls-no-provider`.
pub(crate) async fn fetch_capped(url: &str, max: usize, timeout: Duration) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("build roster poll client")?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .context("GET roster url")?
        .error_for_status()
        .context("roster url status")?;
    // Fast reject on an honest Content-Length; the streaming cap below catches a lying/absent one.
    if let Some(len) = resp.content_length() {
        anyhow::ensure!(
            len as usize <= max,
            "roster body exceeds {max} bytes (content-length {len})"
        );
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.context("read roster url body chunk")? {
        anyhow::ensure!(
            body.len() + chunk.len() <= max,
            "roster body exceeds {max} bytes (streamed)"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Poll the pinned roster URL over TLS ONCE (the HTTPS fallback channel). GET the URL → body
/// bytes → parse the served roster's serial. If it is NEWER than installed, CONVERGE through the
/// host's SAME single install pipeline [`on_announce`] uses (no second validator). On an EQUAL
/// served serial (no new roster) CONFIRM currency ([`DistributionHost::confirm_roster_current`],
/// the freshness bump) — the URL poll is the ONLY channel that confirms freshness without a serial
/// bump — but ONLY when the served body is genuinely the operator's org-root-SIGNED roster
/// (re-verified against the pinned pk, `equal_serial_body_is_authentic`): the HTTPS host is
/// UNTRUSTED, so an unsigned / wrong-org body at the installed serial is logged and IGNORED (the
/// org-root sig is the sole trust input across ALL channels, uniform with the gossip +
/// newer-serial paths, so an unauthenticated body can never forge currency past the staleness
/// fail-safe). A blocked / failed poll returns `Err` (the loop logs it and retries next interval,
/// failing toward degraded); an unparseable body or a stale served serial is logged HERE (at
/// warn / debug) and ignored — the announce/blob/hash are NOT trust inputs; the org-root
/// signature is the sole trust boundary.
///
/// The convergence re-checks `serial > installed` INSIDE the host's single-writer lock
/// (idempotent against a racing gossip announce), and the on-disk stale-serial check is the
/// backstop. The roster path is per-node (derived from the host's config path), consistent with
/// `on_announce` — two daemons in one test process never share a roster file.
pub async fn poll_roster_url_once<H: DistributionHost>(mesh: &Arc<H>, url: &str) -> Result<()> {
    let body = fetch_capped(url, MAX_ROSTER_BYTES, POLL_TIMEOUT)
        .await
        .context("poll roster url")?;
    let now = crate::util::epoch_now_i64();
    let installed = mesh.roster().view().map(|v| v.serial()).unwrap_or(0);
    // Keep the WHOLE parsed roster (not just its serial): the equal-serial confirm re-verifies the
    // served body's org-root signature before it accepts the body as proof of currency.
    let parsed = serde_json::from_slice::<mcpmesh_trust::roster::Roster>(&body).ok();
    let parsed_serial = parsed.as_ref().map(|r| r.serial);
    if let Some(s) = parsed_serial.filter(|s| *s > installed) {
        // Converge through the SHARED single-writer pipeline (the announce side runs the identical
        // thing — the SAME validator; serial re-checked under the lock).
        if mesh.install_roster_bytes(&body, s, "url-poll").await? {
            // Re-seed + re-announce onto gossip too (operator-offline-safe propagation).
            let _ = announce_roster(mesh).await;
        }
    } else if let Some(s) = parsed_serial {
        // `s <= installed` (the newer branch caught `s > installed`).
        if s == installed {
            // Equal serial: CONFIRM currency without a bump — the freshness signal gossip/manual cannot
            // give (they only fire on a NEW serial). But ONLY on org-root-AUTHENTICATED bytes: the
            // pinned HTTPS host is UNTRUSTED, so the served body must re-verify the org-root
            // signature — the SOLE trust input, uniform across ALL channels — against the
            // pinned pk before it can bump `last_confirmed`. Otherwise a compromised/spoofed host could
            // serve an unsigned body at the installed serial and forge currency, defeating the
            // staleness fail-safe. A parse-success is NOT authentication. (Note `installed == 0` — no
            // roster yet — with a served serial 0 never reaches here: a valid roster is serial ≥ 1, so
            // serial 0 lands in the stale branch.)
            if equal_serial_body_is_authentic(&**mesh, parsed.as_ref()) {
                mesh.confirm_roster_current(now).await;
            } else {
                // Fail-safe: DON'T refresh currency from an unauthenticated body — but never degrade or
                // error the node. The existing `last_confirmed` stands and degrades normally if no
                // authenticated confirm arrives. The newer + stale branches are unchanged.
                tracing::warn!(
                    serial = s,
                    "roster URL served an unauthenticated/mismatched body at the installed serial; \
                     not confirming currency (org-root sig is the sole trust input)"
                );
            }
        } else {
            // Stale served serial (`s < installed`): the host is serving an OLDER roster than we hold.
            // Never installed, never confirms — but surface it (a misconfigured/rolled-back host).
            tracing::debug!(
                serial = s,
                installed,
                "roster URL served a stale (older) serial; ignoring"
            );
        }
    } else {
        // The body did not parse as a signed roster: a garbage / misconfigured / wrong-content URL.
        // Was previously a SILENT `Ok(())` — now surfaced at warn so a bad `[roster].url` is visible.
        tracing::warn!("roster URL body did not parse as a signed roster; check [roster].url");
    }
    Ok(())
}

/// Is `parsed` genuinely the operator's org-root-SIGNED roster — the authentication the
/// equal-serial currency confirm REQUIRES before it bumps `last_confirmed`? The pinned HTTPS host
/// is UNTRUSTED; the org-root signature is the SOLE trust input across ALL channels. The gossip +
/// newer-serial paths already enforce it (inside the install pipeline); this closes the
/// equal-serial confirm so a compromised/spoofed host serving an UNSIGNED or WRONG-ORG body at the
/// installed serial cannot forge currency and defeat the staleness fail-safe. Re-runs the EXACT
/// signature verify ([`mcpmesh_trust::roster::sign::verify`]) the install pipeline uses, against
/// the LIVE-pinned org-root pk ([`DistributionHost::pinned_org_root_pk`], read fresh). The caller
/// invokes this only in the equal-serial branch, where the parsed serial already equals the
/// installed serial — so a passing check means the host served genuinely the operator's signed
/// roster AT the installed serial. Returns false — NEVER errors — on a parse-fail body, no pinned
/// pk, an unreadable/invalid pk, or a bad signature; the caller then logs + skips, leaving
/// `last_confirmed` to degrade normally.
fn equal_serial_body_is_authentic<H: DistributionHost>(
    mesh: &H,
    parsed: Option<&mcpmesh_trust::roster::Roster>,
) -> bool {
    let Some(roster) = parsed else {
        return false;
    };
    let Ok(Some(pk_b64)) = mesh.pinned_org_root_pk() else {
        return false;
    };
    let Ok(pk) = crate::roster::parse_org_root_pk(&pk_b64) else {
        return false;
    };
    mcpmesh_trust::roster::sign::verify(roster, &pk).is_ok()
}

/// Spawn the URL-poll loop (the HTTPS fallback channel) — roster mode with a `[roster].url` set.
/// Calls [`poll_roster_url_once`] immediately (so a joiner bootstraps its FIRST roster at startup)
/// and then every `interval_secs` (config `[roster].poll_interval`, default hourly). A failed poll
/// is logged at debug and retried next interval (fails toward degraded, never crashes the loop).
/// The detached handle runs for the daemon lifetime.
pub fn spawn_poll_loop<H: DistributionHost>(
    mesh: Arc<H>,
    url: String,
    interval_secs: i64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(interval_secs.max(1) as u64);
        loop {
            if let Err(e) = poll_roster_url_once(&mesh, &url).await {
                // A SIGNATURE or FORMAT failure is not a transient poll error and must not be
                // logged as one (#93 ask c gate): those are the two shapes a node stuck behind an
                // org-root rotation sees, and it will retry against a document it can never verify
                // until the roster expires — which is precisely the "90 days later, nobody connects
                // cause to effect" failure #93 opens with. Say so, once per poll, at `warn!`.
                let msg = format!("{e:#}");
                if msg.contains("signature") || msg.contains("roster format") {
                    tracing::warn!(
                        %e,
                        "roster poll REJECTED the served roster — if your org rotated its root \
                         while this node was off, or this node is behind two rotations, re-run \
                         `mcpmesh join` with a current org invite. Retrying will not help"
                    );
                } else {
                    tracing::debug!(%e, "roster URL poll failed; will retry next interval");
                }
            }
            tokio::time::sleep(period).await;
        }
    })
}

/// Spawn the roster-topic receive loop: pull announcements off the receiver and dispatch each to
/// [`on_announce`]. Runs for the daemon lifetime (the loop ends when the topic stream closes). A
/// malformed payload is dropped without a panic (the receive path is fed arbitrary peer bytes); a
/// `None` receiver (pure-pairing daemon, or already taken) returns immediately.
pub fn spawn_receive_loop<H: DistributionHost>(mesh: Arc<H>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(mut receiver) = mesh.take_roster_topic_receiver().await else {
            return;
        };
        // Bounded off-loop fetch concurrency: a stalling provider must not wedge the
        // loop, so each accepted announce SPAWNS its fetch+install holding one of these permits.
        let fetch_slots =
            std::sync::Arc::new(tokio::sync::Semaphore::new(GOSSIP_FETCH_CONCURRENCY));
        // Single-consumer announce rate bucket (this loop is the ONLY consumer — a plain owned bucket,
        // no lock). Bounds how often we TRIGGER a fetch.
        let mut announce_bucket = crate::limits::TokenBucket::new(
            f64::from(GOSSIP_ANNOUNCE_PER_MIN),
            f64::from(GOSSIP_ANNOUNCE_PER_MIN) / 60.0,
            Instant::now(),
        );
        while let Some(content) = transport::next_message(&mut receiver).await {
            let announce = match RosterAnnounce::from_bytes(&content) {
                Ok(a) => a,
                Err(e) => {
                    tracing::debug!(%e, "malformed roster announce dropped");
                    continue;
                }
            };
            // Rate-limit announce PROCESSING (fail-safe drop over-limit).
            if announce_bucket.try_take(Instant::now()).is_err() {
                tracing::debug!("gossip announce rate limit engaged; dropping announce");
                continue;
            }
            // Bounded concurrency: a free permit → spawn the fetch+install; a full pool DROPS the
            // announce (a re-announce / the URL poll re-converges). The loop keeps pulling announces.
            // `on_announce` still installs under the host's single-writer lock with a
            // `serial > installed` recheck, so concurrent handlers are idempotent (single-convergence
            // preserved) and that lock is only ever a source here (no cycle).
            let Ok(permit) = fetch_slots.clone().try_acquire_owned() else {
                tracing::debug!("gossip fetch pool full; dropping announce (will re-converge)");
                continue;
            };
            let mesh2 = mesh.clone();
            tokio::spawn(async move {
                let _permit = permit; // held for the whole fetch+install
                if let Err(e) = on_announce(&mesh2, announce).await {
                    tracing::debug!(%e, "gossip roster announce handling failed");
                }
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A one-shot HTTP/1.1 server: sends `status` + `body`, optionally sleeping first (a hung host).
    fn serve_once(body: Vec<u8>, sleep_ms: u64) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                if sleep_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/roster.json")
    }

    /// reqwest 0.13.4 (`default-features=false, rustls-no-provider`) resolves the rustls
    /// `CryptoProvider` at `Client::builder().build()` and PANICS ("No rustls crypto provider is
    /// configured") if none is installed — scheme-INDEPENDENT, so even an http:// test URL panics.
    /// Same requirement as the existing `tests/roster_distribute.rs` polls; the daemon's
    /// `serve_forever` installs it process-wide once. `install_default` errors if already installed,
    /// so `let _ =` makes repeated test calls idempotent.
    fn install_ring() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[tokio::test]
    async fn fetch_capped_reads_a_small_body() {
        install_ring();
        let url = serve_once(b"{\"format\":\"mcpmesh-roster/1\"}".to_vec(), 0);
        let got = fetch_capped(&url, 1024, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(got.starts_with(b"{\"format\""));
    }

    #[tokio::test]
    async fn fetch_capped_rejects_an_oversized_body_without_oom() {
        install_ring();
        // 2 MiB body, cap 64 KiB → rejected before the whole body is buffered.
        let url = serve_once(vec![b'x'; 2 * 1024 * 1024], 0);
        let err = fetch_capped(&url, 64 * 1024, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("exceeds"),
            "size cap rejects: {err:#}"
        );
    }

    #[tokio::test]
    async fn fetch_capped_times_out_a_hung_host() {
        install_ring();
        let url = serve_once(b"late".to_vec(), 2000);
        let err = fetch_capped(&url, 1024, Duration::from_millis(200))
            .await
            .unwrap_err();
        // A timeout is an Err (the poll loop logs + retries next interval, failing toward degraded).
        let _ = err;
    }
}
