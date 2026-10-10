//! Roster/presence gossip + blob transport. Thin wrappers
//! over iroh-gossip (=0.101.0) + iroh-blobs (=0.103.0) sharing the daemon's ONE endpoint
//! (). The gossip TOPICS are deterministic blake3 derivations of the org id
//! — pure + unit-testable here.

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::{Endpoint, EndpointId};
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::ticket::BlobTicket;
use iroh_blobs::{BlobFormat, BlobsProtocol};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};

/// The roster-distribution gossip topic bytes: `blake3("mcpmesh/roster/" + org_id)`.
/// Pure — returns the 32 bytes a `TopicId::from_bytes` wraps, so the derivation is testable
/// without any gossip runtime.
pub fn roster_topic_bytes(org_id: &str) -> [u8; 32] {
    *blake3::hash(format!("mcpmesh/roster/{org_id}").as_bytes()).as_bytes()
}

/// The presence gossip topic bytes: `blake3("mcpmesh/presence/" + org_id)`.
pub fn presence_topic_bytes(org_id: &str) -> [u8; 32] {
    *blake3::hash(format!("mcpmesh/presence/{org_id}").as_bytes()).as_bytes()
}

/// The roster-distribution gossip announcement: a higher `serial` tells receivers a
/// newer roster exists; `roster_hash` (blake3, `"blake3:<hex>"`) binds the announce to the exact
/// document; `blob_ticket` is the iroh-blobs fetch handle. Content-addressed + org-root-SIGNED, so
/// this announce is not itself a trust input — it only triggers a fetch that `validate_for_install`
/// then judges (the SINGLE convergence point). Kept ≤ 512 B (small-payload gossip discipline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterAnnounce {
    pub serial: u64,
    pub roster_hash: String, // "blake3:<hex>"
    pub blob_ticket: String, // iroh-blobs BlobTicket string
}

impl RosterAnnounce {
    /// Serialize to the compact JSON gossip payload. Infallible for this fixed shape (u64 + two
    /// Strings always encode), so an `expect` here signals a serde-internal bug, not runtime data.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("RosterAnnounce serializes")
    }

    /// Parse a received gossip payload. NEVER panics on hostile/garbage input — the gossip receive
    /// path is fed arbitrary peer bytes, so a malformed payload is a typed `Err` the caller drops.
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        serde_json::from_slice(b).context("parse roster announce")
    }
}

/// A subscribed gossip topic split into its send + receive halves — the roster daemon holds one for
/// the roster topic (announce + converge) and one for the presence topic (heartbeat + track). The
/// `sender` is cheaply cloneable (announce + re-announce from many sites); the `receiver` is a
/// single-consumer stream, so it is `Option`-wrapped and moved out EXACTLY ONCE by the daemon's
/// receive loop (`MeshState::take_roster_topic_receiver`), leaving the sender live for announce.
pub struct RosterGossip {
    pub sender: GossipSender,
    pub receiver: Option<GossipReceiver>,
}

/// Spawn iroh-gossip on the daemon's SHARED endpoint (). Returns the `Gossip`
/// handle: the caller registers `GOSSIP_ALPN` dispatch in the accept loop and subscribes to
/// topics (below). One `Gossip` per daemon serves BOTH the roster and presence topics.
///
/// `Gossip::builder().spawn(endpoint)` returns the handle directly (not a `Result`) — a closed
/// endpoint degrades the actor rather than failing the spawn (verified against iroh-gossip net.rs).
pub fn spawn_gossip(endpoint: &Endpoint) -> Gossip {
    Gossip::builder().spawn(endpoint.clone())
}

/// Subscribe to a topic (roster or presence), bootstrapping from the roster-listed peers. Uses
/// `subscribe` (NOT `subscribe_and_join`) so the FIRST node does not block awaiting a neighbor
/// (); the swarm forms as peers come online. An empty `bootstrap` is fine.
pub async fn subscribe(
    gossip: &Gossip,
    topic: [u8; 32],
    bootstrap: Vec<EndpointId>,
) -> Result<RosterGossip> {
    let topic_id = TopicId::from_bytes(topic);
    let topic = gossip
        .subscribe(topic_id, bootstrap)
        .await
        .context("subscribe to gossip topic")?;
    let (sender, receiver) = topic.split();
    Ok(RosterGossip {
        sender,
        receiver: Some(receiver),
    })
}

/// Broadcast a raw payload on a subscribed topic's sender (a roster announce, or a presence beat).
pub async fn broadcast(sender: &GossipSender, payload: Vec<u8>) -> Result<()> {
    sender
        .broadcast(Bytes::from(payload))
        .await
        .context("broadcast gossip message")
}

/// Pull the next RECEIVED gossip payload (message content) off a topic's receiver, skipping
/// membership events (`NeighborUp`/`Down`/`Lagged`). Returns `None` when the topic stream ends.
pub async fn next_message(receiver: &mut GossipReceiver) -> Option<Bytes> {
    while let Some(ev) = receiver.next().await {
        if let Ok(Event::Received(msg)) = ev {
            return Some(msg.content);
        }
        // NeighborUp/Down/Lagged and receive errors: keep reading (advisory, best-effort).
    }
    None
}

/// Re-export the gossip ALPN so the accept-loop dispatch matches it without importing the crate
/// at the daemon site — one vocabulary home (cli-plumbing precedent).
pub const GOSSIP_ALPN: &[u8] = iroh_gossip::ALPN;

/// The iroh-blobs ALPN (`b"/iroh-bytes/4"`) — re-exported so the accept-loop dispatch matches
/// it without importing the crate at the daemon site (the same one-vocabulary-home discipline as
/// `GOSSIP_ALPN`).
pub const BLOB_ALPN: &[u8] = iroh_blobs::ALPN;

/// The roster-blob transport: a `MemStore`-backed provider+fetcher for the SIGNED
/// roster document ONLY. iroh-blobs is content-addressed and BLAKE3-verifies every transfer against
/// the ticket's hash; `fetch` adds a SECOND explicit blake3 check tying the fetched doc to the
/// gossip announce's `roster_hash` (the announce-to-document binding). NO gated per-scope provider
/// here (the app-blob provider is separate — hence `BlobsProtocol::new(store, None)`, no
/// `EventSender`). `MemStore` (not an
/// `FsStore`): the roster blob is small + re-seeded from the installed `roster.json` on every
/// restart, so no durable blob store is needed here ().
#[derive(Clone)]
pub struct RosterBlobs {
    store: MemStore,
}

impl RosterBlobs {
    /// A fresh `MemStore`-backed transport. The endpoint is threaded through `publish`/`fetch`/
    /// `spawn_accept` (not stored) so this shares the daemon's ONE endpoint ().
    pub fn new(_endpoint: &Endpoint) -> Self {
        Self {
            store: MemStore::new(),
        }
    }

    /// The `BlobsProtocol` handler the accept loop dispatches the blob ALPN to. Ungated
    /// (`None` events) — the identity gate on the accept-loop arm is the access boundary, not the
    /// blob provider. `&self.store` (a `&MemStore`) deref-coerces to the `&Store` `new` expects.
    pub fn protocol(&self) -> BlobsProtocol {
        BlobsProtocol::new(&self.store, None)
    }

    /// TEST-ONLY: register a blob ALPN accept handler directly on `endpoint`, BYPASSING the trust
    /// gate. Production accept ALWAYS goes through the gated daemon loop (`spawn_accept_loop`'s
    /// `BLOB_ALPN` arm: resolve → 401 + check-register); this exists only so same-file unit
    /// tests can serve a blob without assembling a daemon. `#[cfg(test)]` so it can never leak
    /// into a production accept path.
    #[cfg(test)]
    pub(crate) fn spawn_accept(&self, endpoint: &Endpoint) {
        let proto = self.protocol();
        let ep = endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                if let Ok(conn) = incoming.await
                    && conn.alpn() == BLOB_ALPN
                {
                    let _ = iroh::protocol::ProtocolHandler::accept(&proto, conn).await;
                }
            }
        });
    }

    /// Add the signed roster bytes to the store, returning `(ticket_string, "blake3:<hex>")`. The
    /// operator (on publish) + every accepting node (re-seed) call this so the blob is servable
    /// onward independent of the operator staying online. `add_bytes(..)
    /// .await` yields a `TagInfo` (a PERSISTENT named tag — the blob is NOT GC-eligible, so the
    /// provider keeps serving it), whose `.hash` field is the blake3 root the ticket pins.
    pub async fn publish(&self, doc: &[u8], endpoint: &Endpoint) -> Result<(String, String)> {
        let tag = self
            .store
            .blobs()
            .add_bytes(doc.to_vec())
            .await
            .context("add roster blob")?;
        let ticket = BlobTicket::new(endpoint.addr(), tag.hash, BlobFormat::Raw);
        let hash_hex = format!("blake3:{}", blake3::hash(doc).to_hex());
        Ok((ticket.to_string(), hash_hex))
    }

    /// Fetch a roster blob by its announce ticket, VERIFY it against the announce's `roster_hash`,
    /// and return the bytes. TWO independent integrity layers guard the returned document. FIRST,
    /// iroh-blobs is content-addressed — the `Downloader` BLAKE3-verifies the transferred data
    /// against the ticket's pinned hash, so a corrupt/substituted transfer fails the download.
    /// SECOND, this method recomputes `blake3(bytes)` and REJECTS a mismatch against the announce's
    /// `roster_hash`, binding the fetched document to what the signed-announce chain named
    /// — a distributor never `validate_for_install`s a doc the announce did not name.
    ///
    /// The roster's OWN org-root signature is judged later by `validate_for_install` — this returns
    /// bytes, not trust.
    pub async fn fetch(
        &self,
        ticket_str: &str,
        roster_hash: &str,
        endpoint: &Endpoint,
    ) -> Result<Vec<u8>> {
        let ticket: BlobTicket = ticket_str.parse().context("parse blob ticket")?;
        // the `Downloader`'s `ContentDiscovery` bound maps providers to
        // `EndpointId` (NOT `EndpointAddr` — `EndpointAddr: !Into<EndpointId>`), so pass the ticket
        // addr's `.id`; the transport address is resolved via the endpoint's address lookup
        // (MemoryLookup in tests, DNS/pkarr in prod), matching the id-only dial the codebase uses.
        // `ticket.hash()` (a `Hash`) satisfies `SupportedRequest`; `ticket.addr()` (NOT
        // `endpoint_addr()`) is the provider `EndpointAddr`.
        self.store
            .downloader(endpoint)
            .download(ticket.hash(), Some(ticket.addr().id))
            .await
            .context("download roster blob")?;
        let bytes = self
            .store
            .get_bytes(ticket.hash())
            .await
            .context("read fetched roster blob")?
            .to_vec();
        // Layer 2: the explicit announce-to-document binding — reject a doc that is not what the
        // announce's `roster_hash` named, BEFORE any bytes are returned to the caller.
        let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        if got != roster_hash {
            anyhow::bail!("roster blob hash mismatch: announce {roster_hash}, fetched {got}");
        }
        Ok(bytes)
    }
}

/// A BOUNDED provider address book for roster-blob fetches (no unbounded memory from
/// announces). Wraps ONE `MemoryLookup` registered on the endpoint at startup; `note` records a
/// provider's addr, but only for a NEW id and only up to `cap` distinct ids — so the address book
/// (and the `known` set) is bounded by distinct providers (≈ roster size), never per-announce.
///
/// rests on `MemoryLookup` being `Clone` (shared inner state, so the registered clone
/// sees later `add_endpoint_info`) and de-duplicating by endpoint id. Verify vs the pinned iroh; if
/// `MemoryLookup` is not `Clone`, hold the sole instance here and register it lazily on first `note`.
pub struct RosterAddrBook {
    lookup: iroh::address_lookup::MemoryLookup,
    known: std::sync::Mutex<std::collections::HashSet<[u8; 32]>>,
    cap: usize,
}

impl RosterAddrBook {
    /// Build the book and REGISTER its single `MemoryLookup` on `endpoint` (once).
    pub fn register(endpoint: &iroh::Endpoint, cap: usize) -> Self {
        let lookup = iroh::address_lookup::MemoryLookup::new();
        if let Ok(al) = endpoint.address_lookup() {
            al.add(lookup.clone());
        }
        Self {
            lookup,
            known: std::sync::Mutex::new(std::collections::HashSet::new()),
            cap,
        }
    }

    /// Note a provider's addr for resolution — BOUNDED: only a NEW id under the cap is added. Returns
    /// whether it was newly added (the test's bound assertion + fail-safe: a full book keeps serving
    /// with the providers it already knows).
    pub fn note(&self, addr: iroh::EndpointAddr) -> bool {
        // #203: what lands here is a GOSSIP-supplied blob ticket's address — a remote party's
        // claim, recorded BEFORE the roster blob is fetched and long before its org-root signature
        // is checked. Everything recorded becomes a candidate path iroh sends padded QUIC Initials
        // to: the same exposure as the pairing invite, on a site 0.52.1's filter missed entirely.
        //
        // Filtered HERE, at the book's own entry point, so it holds for every caller — and so the
        // effect is observable through `recorded_for`, which is what makes the call site testable
        // rather than only the helper.
        //
        // **RELAY URLs are NOT filtered**, and at this site that is worse than at the pairing
        // invite: `dialable_only` passes every non-`Ip` variant, iroh opens an outbound TLS
        // connection to any relay URL it is handed, and unlike a redemption this path is automatic,
        // unattended and repeatable at the announce rate limit. #203 calls that the sharper edge and
        // it is not addressed here.
        //
        // The `cap` below bounds distinct IDS, not addresses per id. That gap is #203's, not this
        // function's: bounding addresses means CHOOSING which to keep, and choosing wrong drops the
        // ones that work — a lesson from an abandoned attempt at exactly that.
        let addr = crate::daemon::dial::dialable_only(addr);
        let id = *addr.id.as_bytes();
        let mut known = self.known.lock().expect("roster addr book mutex");
        if known.contains(&id) || known.len() >= self.cap {
            return false;
        }
        known.insert(id);
        self.lookup.add_endpoint_info(addr);
        true
    }

    /// Distinct providers currently known (the bounded-memory assertion reads this).
    pub fn known_len(&self) -> usize {
        self.known.lock().expect("roster addr book mutex").len()
    }

    /// What this book actually handed to address lookup for `id`.
    ///
    /// `#[doc(hidden)]` — a TEST SEAM (#203), so a test can assert the CALL SITE rather than the
    /// filter helper. It reads `MemoryLookup` itself rather than a copy: an earlier version kept a
    /// duplicate `EndpointAddr` per id on the premise that `MemoryLookup` had no reader, which is
    /// false — `get_endpoint_info` exists — so the duplicate was unnecessary state AND observed a
    /// sibling rather than the sink it was meant to prove.
    #[doc(hidden)]
    pub fn recorded_for(&self, id: &[u8; 32]) -> Option<iroh::EndpointAddr> {
        let eid = iroh::EndpointId::from_bytes(id).ok()?;
        self.lookup
            .get_endpoint_info(eid)
            .map(iroh::address_lookup::EndpointInfo::into_endpoint_addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #203: a gossip-supplied ticket cannot seed an un-dialable address into address lookup.
    ///
    /// `note` is fed from `on_announce`, which parses a ticket off the gossip topic and records its
    /// `EndpointAddr` BEFORE the roster blob is fetched and long before its org-root signature is
    /// checked. Everything recorded becomes a candidate path iroh sends padded QUIC Initials to —
    /// the same exposure as the pairing invite, on a site 0.52.1's filter missed entirely.
    ///
    /// The book has no reader for its addresses, so this asserts through the OBSERVABLE effect: an
    /// entry whose every address is un-dialable degrades to a bare-id record, which is what
    /// `dialable_only` produces and what a dial then treats as "discovery only".
    #[tokio::test]
    async fn a_gossiped_ticket_address_is_filtered_before_it_reaches_address_lookup() {
        let id = iroh::SecretKey::from_bytes(&[21u8; 32]).public();
        let hostile = iroh::EndpointAddr::from_parts(
            id,
            [
                iroh::TransportAddr::Ip("0.0.0.0:53".parse().unwrap()),
                iroh::TransportAddr::Ip("224.0.0.1:1900".parse().unwrap()),
                iroh::TransportAddr::Ip("192.168.1.5:4433".parse().unwrap()),
            ],
        );
        // Asserted through what the book RECORDED, not through the filter helper — the helper
        // passing says nothing about whether this call site invokes it.
        // A real endpoint, since `register` installs the lookup on one. Relay-disabled so the
        // test binds nothing beyond loopback.
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .expect("bind");
        let book = RosterAddrBook::register(&ep, 8);
        assert!(
            book.note(hostile),
            "a ticket with a usable address is still noted"
        );
        assert_eq!(book.known_len(), 1);
        let recorded = book
            .recorded_for(id.as_bytes())
            .expect("the provider was recorded");
        assert_eq!(
            recorded.addrs.len(),
            1,
            "only the dialable address may reach address lookup: {recorded:?}"
        );
        assert!(
            recorded.addrs.iter().all(|a| matches!(
                a,
                iroh::TransportAddr::Ip(s) if s.to_string() == "192.168.1.5:4433"
            )),
            "and it is the REAL one, not whichever survived by accident: {recorded:?}"
        );

        // An all-hostile ticket is still noted (the provider id is real and the fetch may still
        // resolve by discovery) — it simply contributes no destinations.
        let all_bad = iroh::EndpointAddr::from_parts(
            iroh::SecretKey::from_bytes(&[22u8; 32]).public(),
            [iroh::TransportAddr::Ip("0.0.0.0:1".parse().unwrap())],
        );
        assert!(book.note(all_bad));
        assert_eq!(book.known_len(), 2);
    }

    #[test]
    fn topics_are_deterministic_distinct_and_org_scoped() {
        // Deterministic: same org → same bytes.
        assert_eq!(roster_topic_bytes("acme"), roster_topic_bytes("acme"));
        // The roster and presence topics for one org are DISTINCT (different domain prefixes).
        assert_ne!(roster_topic_bytes("acme"), presence_topic_bytes("acme"));
        // Org-scoped: a different org yields a different roster topic (no cross-org bleed, NG3).
        assert_ne!(roster_topic_bytes("acme"), roster_topic_bytes("globex"));
        // Exactly the spec's derivation (belt-and-suspenders: recompute the literal).
        assert_eq!(
            roster_topic_bytes("acme"),
            *blake3::hash(b"mcpmesh/roster/acme").as_bytes()
        );
    }

    #[test]
    fn roster_announce_round_trips_json() {
        // The gossip payload: serial + roster_hash (blake3 hex) + blob_ticket (opaque).
        let a = RosterAnnounce {
            serial: 42,
            roster_hash: "blake3:deadbeef".into(),
            blob_ticket: "blobabc123".into(),
        };
        let bytes = a.to_bytes();
        let back = RosterAnnounce::from_bytes(&bytes).expect("valid announce");
        assert_eq!(back, a);
        // Hostile/short input Errs (never panics) — the gossip receive path must tolerate garbage.
        assert!(RosterAnnounce::from_bytes(b"not json").is_err());
        // The payload is small (well under the 512B presence cap / 4KiB gossip default).
        assert!(
            bytes.len() < 512,
            "announce is compact: {} bytes",
            bytes.len()
        );
    }

    #[tokio::test]
    async fn blob_add_ticket_and_fetch_round_trips_with_hash_check() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            // Provider endpoint (localhost, relay-disabled) + a MemStore-backed RosterBlobs.
            let provider_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(vec![GOSSIP_ALPN.to_vec(), BLOB_ALPN.to_vec()])
                .bind()
                .await
                .unwrap();
            let provider = RosterBlobs::new(&provider_ep);
            provider.spawn_accept(&provider_ep); // serve the blob ALPN in-process

            let doc = br#"{"format":"mcpmesh-roster/1","serial":7}"#.to_vec();
            let (ticket, hash_hex) = provider.publish(&doc, &provider_ep).await.unwrap();

            // Fetcher endpoint seeded with the provider's addr (localhost has no discovery).
            // the settled iroh 1.0.1 names are `add_endpoint_info`
            // (NOT `add_address`) + `address_lookup() -> Result<_>` with a synchronous `add`
            // (NOT an async `.add(..).await`) — mirrors `cli/tests/hero_flow.rs`.
            let fetcher_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .bind()
                .await
                .unwrap();
            let mem = iroh::address_lookup::MemoryLookup::new();
            mem.add_endpoint_info(provider_ep.addr());
            fetcher_ep
                .address_lookup()
                .expect("address lookup services")
                .add(mem);
            let fetcher = RosterBlobs::new(&fetcher_ep);

            let fetched = fetcher
                .fetch(&ticket, &hash_hex, &fetcher_ep)
                .await
                .unwrap();
            assert_eq!(fetched, doc, "blob fetch returns the exact bytes");

            // A hash MISMATCH is rejected (the announce-to-document binding).
            assert!(
                fetcher
                    .fetch(&ticket, "blake3:0000", &fetcher_ep)
                    .await
                    .is_err(),
                "a wrong roster_hash rejects the fetched blob"
            );
        })
        .await
        .expect("blob round-trip timed out");
    }

    #[tokio::test]
    async fn roster_addr_book_dedups_and_caps() {
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let p1 = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let p2 = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        // cap = 1: the first distinct provider is added; a second distinct one is refused (bounded).
        let book = RosterAddrBook::register(&ep, 1);
        assert!(book.note(p1.addr()), "first provider added");
        assert!(
            !book.note(p1.addr()),
            "same provider is a no-op (dedup by id)"
        );
        assert_eq!(book.known_len(), 1);
        assert!(
            !book.note(p2.addr()),
            "second distinct provider refused at the cap (bounded)"
        );
        assert_eq!(book.known_len(), 1, "the known set never exceeds the cap");
    }
}
