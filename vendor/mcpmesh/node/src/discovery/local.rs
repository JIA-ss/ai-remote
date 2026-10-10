//! LOCAL (mDNS) peer discovery (#68) — find peers on the same link with no internet at all.
//!
//! # Why this exists
//!
//! Peer resolution otherwise depends on external infrastructure: the pkarr publisher/resolver a
//! relay provides, or a dialable address someone already handed over in an invite. Two machines on
//! the same LAN with no uplink cannot find each other, though the network path between them is
//! fine. That is the scenario where "peer to peer" earns its keep — a boat, a workshop, a failed
//! uplink, a deliberately air-gapped network — and the commoner weak version too: a LAN where the
//! internet is merely flaky, so peers that could talk directly fail to resolve because resolution
//! goes out first.
//!
//! # This is a DEPENDENCY, not an implementation
//!
//! #68 concluded "there is no mDNS in iroh 1.0.3, so this needs an implementation", from a correct
//! reading of `iroh-1.0.3/src/address_lookup/` — which contains exactly `dns.rs`, `memory.rs` and
//! `pkarr.rs`. iroh 1.x did not drop mDNS; it moved it into a companion crate, and says so in the
//! module docs of that same file:
//!
//! > mDNS-based and Mainline-DHT-based Address Lookup services live in separate crates:
//! > `iroh-mdns-address-lookup` and `iroh-mainline-address-lookup`.
//!
//! So this module is thin on purpose. An mDNS responder is a multicast listener on every interface;
//! one written here would be ours to get right and ours to keep right, against a transport whose
//! address model it has to track. n0's stays version-matched to the iroh we pin.
//!
//! # What it discloses
//!
//! Advertising multicasts this node's endpoint id and its addresses to **every device on the link**,
//! unprompted and repeatedly, including machines that had no idea it existed. "Its addresses" means
//! the LAN address, the PUBLIC WAN IPv4 and global IPv6 — a café LAN learns your home/ISP address,
//! not merely that you are there. That is why `[network].local_discovery` defaults to `"off"`, a
//! deliberate departure from what #68 asked for.
//!
//! **`"resolve"` is quieter, not silent.** Resolving over mDNS means asking:
//! `MdnsAddressLookup` builds a `Discoverer::new_interactive` unconditionally (τ = 700 ms) and
//! `advertise` gates only `with_addrs`, so a resolving node multicasts a `_mcpmesh._udp.local`
//! query roughly once a second for as long as it runs. It publishes no identity and no addresses —
//! pinned on the wire — but the query itself says "an mcpmesh node is at this IP, right now". The
//! docs said "listen only" until the 0.44.0 gate captured the packets.
//!
//! **`relay_only` does not restrain any of this on a stock build.** `AddrFilter::relay_only()` is
//! installed only under the `unstable-relay-only` feature; without it the filter does not exist and
//! the full direct address set goes out. Boot warns, naming which of the two builds the operator
//! has. An earlier version of this comment asserted the filter always applied; it does not.

use iroh::EndpointId;
use iroh::address_lookup::AddressLookup;
use iroh_mdns_address_lookup::MdnsAddressLookup;

/// The mDNS service name mcpmesh nodes announce and listen on.
///
/// Deliberately NOT the crate's `irohv1` default, which is the SHARED iroh namespace: every iroh
/// application on the link would advertise into it and be resolved out of it. That is not an
/// authorization problem — resolution answers *where*, never *who may*, and a peer found this way
/// still faces the trust gate — but it is a disclosure and a noise problem, announcing this node to
/// unrelated applications and resolving endpoint ids that can never be ours.
///
/// Records take the form `<endpoint-id>._mcpmesh._udp.local`.
pub const SERVICE_NAME: &str = "mcpmesh";

/// Build the local-discovery lookup for `endpoint_id`.
///
/// **Takes the parsed [`LocalDiscovery`], not a bare `bool`, deliberately.** The 0.44.0 gate
/// mutated the call site from `local_disc.advertise` to a literal `true` — putting a node told to
/// listen only on the air, broadcasting its endpoint id — and every deterministic test stayed
/// green. A bare bool makes that a one-character edit; a struct parsed from config makes the same
/// lie require constructing a `LocalDiscovery` that disagrees with the operator's file, which is
/// visible at review. It does not make the mistake impossible, and the wire test below is what
/// actually pins it.
///
/// Fallible because a machine with no multicast-capable interface cannot run this; boot warns and
/// continues rather than refusing to start, since that is a networking condition rather than a
/// misconfiguration.
///
/// Must be called from within a tokio runtime: the crate's builder relies on
/// `Handle::current()` and PANICS otherwise.
pub fn build(
    endpoint_id: EndpointId,
    mode: crate::daemon::boot::LocalDiscovery,
) -> anyhow::Result<impl AddressLookup> {
    MdnsAddressLookup::builder()
        .advertise(mode.advertise)
        .service_name(SERVICE_NAME)
        .build(endpoint_id)
        .map_err(|e| anyhow::anyhow!("start local discovery: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The service name must stay `mcpmesh`, not the crate's shared `irohv1` default.
    ///
    /// Asserted as a CONSTANT, which catches a change to the constant and NOTHING ELSE. The 0.44.0
    /// gate deleted the `.service_name(..)` call entirely: this test stayed green, every boot test
    /// stayed green, and even the real-multicast test passed — both nodes simply fell into iroh's
    /// shared `irohv1` namespace together, with zero `_mcpmesh` packets on the wire. That is pinned
    /// where it is observable: `local_discovery_announces_only_under_the_mcpmesh_service_name` in
    /// `cli/tests/embedded_loopback.rs` reads the multicast group directly.
    #[test]
    fn the_service_name_is_ours_and_not_irohs_shared_default() {
        assert_eq!(SERVICE_NAME, "mcpmesh");
        assert_ne!(
            SERVICE_NAME, "irohv1",
            "the crate's default is the SHARED iroh namespace — using it would announce this node \
             to every unrelated iroh app on the link"
        );
    }

    /// Building must not panic inside a runtime, in either mode.
    ///
    /// **This is nearly all it proves, and the 0.44.0 gate said so.** It does not observe the
    /// service name, and it does not observe whether `advertise` reached the socket — deleting
    /// `.service_name(..)` or hard-coding `advertise: true` both leave it green. Those are pinned
    /// on the wire, in `cli/tests/embedded_loopback.rs`'s `#[ignore]`d multicast tests, because
    /// the crate exposes no way to read either back.
    ///
    /// What it does pin is real and load-bearing: the crate PANICS when built outside a tokio
    /// runtime, which would take the whole daemon down at boot rather than warn.
    #[tokio::test]
    async fn both_modes_build_inside_a_runtime() {
        let id = iroh::SecretKey::generate().public();
        for advertise in [true, false] {
            let mode = crate::daemon::boot::LocalDiscovery {
                enabled: true,
                advertise,
            };
            // A machine with no multicast interface legitimately fails; that is the case boot
            // warns about, and it must be an Err rather than a panic.
            match build(id, mode) {
                Ok(_) => {}
                Err(e) => {
                    let msg = format!("{e:#}");
                    assert!(
                        msg.contains("start local discovery"),
                        "a build failure must be contextualized so the boot warning is readable: \
                         {msg}"
                    );
                }
            }
        }
    }
}
