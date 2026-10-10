//! Ingress filtering for address-lookup results (#203).
//!
//! Every other fix for #203 filtered an address blob at a CALL SITE — the pairing invite, the
//! attestation offer, a gossip ticket, a blob ticket. Review established that this cannot bound the
//! problem: `Endpoint::connect` and `blobs.fetch` resolve a peer **by id**, which triggers address
//! lookup, and on the default n0 preset that means pkarr and DNS. **A pkarr record is self-signed by
//! the endpoint key**, so anyone who generates a key can publish an arbitrary address list under it
//! and have every entry inserted into iroh's path set — `insert_multiple(addrs, Source::AddressLookup)`
//! — with no hygiene whatever. iroh has no destination filter of its own: its `AddrFilter` is
//! publish-side only, by design.
//!
//! So this is the layer that actually bounds it. [`Hygienic`] wraps an [`AddressLookup`] and strips
//! every resolved address that can never be a unicast QUIC peer, using the same
//! [`dialable_only`](crate::daemon::dial::dialable_only) predicate the dial paths use — one rule,
//! applied at ingress instead of at each of four call sites.
//!
//! **It wraps RESOLVERS, not the publisher.** `publish` carries our own addresses outward and is
//! already governed by `AddrFilter`; filtering it here would be filtering ourselves.
//!
//! **What it does NOT do**, so the gap is not mistaken for closed: relay URLs pass, exactly as they
//! do at every other site. iroh opens an outbound TLS connection to any relay URL it is handed, and
//! deciding which relay URLs are acceptable needs a provenance model rather than a destination
//! predicate — #203's remaining item.

use iroh::address_lookup::{
    AddressLookup, AddressLookupBuilder, AddressLookupBuilderError, EndpointData, EndpointInfo,
    Item,
};

/// Wrap an address-lookup BUILDER so the service it builds filters what it resolves.
#[derive(Debug)]
pub(crate) struct Hygienic<B>(pub(crate) B);

impl<B: AddressLookupBuilder> AddressLookupBuilder for Hygienic<B> {
    fn into_address_lookup(
        self,
        endpoint: &iroh::Endpoint,
    ) -> Result<impl AddressLookup, AddressLookupBuilderError> {
        Ok(HygienicLookup(self.0.into_address_lookup(endpoint)?))
    }
}

/// Wrap an already-built [`AddressLookup`] — for services added AFTER `bind`, which cannot go
/// through the builder. mDNS local discovery is one, and it is the ingress with the weakest
/// precondition of the three: an mDNS answer is unauthenticated and endpoint ids are public.
pub(crate) fn wrap<L: AddressLookup>(inner: L) -> impl AddressLookup {
    HygienicLookup(inner)
}

/// The built service: delegates everything, filters what `resolve` yields.
#[derive(Debug)]
struct HygienicLookup<L>(L);

impl<L: AddressLookup> AddressLookup for HygienicLookup<L> {
    fn publish(&self, data: &EndpointData) {
        // Outbound: our own addresses, already governed by `AddrFilter`. Untouched.
        self.0.publish(data);
    }

    fn resolve(
        &self,
        endpoint_id: iroh::EndpointId,
    ) -> Option<n0_future::boxed::BoxStream<Result<Item, iroh::address_lookup::Error>>> {
        let inner = self.0.resolve(endpoint_id)?;
        Some(Box::pin(n0_future::StreamExt::map(inner, |r| {
            r.map(filter_item)
        })))
    }
}

/// Strip a resolved item of every address that can never be a unicast QUIC peer.
///
/// Filters the address vector IN PLACE ORDER rather than round-tripping through `EndpointAddr`.
/// That matters twice, and the first version did neither:
///
/// - **Order is load-bearing.** iroh-dns documents `EndpointData.addrs` as ordered, "so it can
///   encode priority for address lookup services, should they not fit into e.g. a single DNS
///   packet". `EndpointAddr.addrs` is a `BTreeSet`, so a round trip silently re-sorts to
///   relay-first-then-numeric — and BTreeSet ordering is exactly what sank an earlier attempt in
///   this area, which capped by prefix and threw away the LAN address and every IPv6.
/// - **`user_data` cannot survive a round trip at all.** `From<EndpointAddr> for EndpointData`
///   hardcodes `user_data: None`, so preserving it explicitly (as the first version did) restored a
///   field the conversion had just discarded — and no fixture built that way could ever prove it.
///
/// Relay entries are kept: `is_dialable_addr` passes every non-`Ip` variant, and dropping them here
/// would disable relay-mediated connectivity for every peer resolved through pkarr, DNS or mDNS.
pub(crate) fn filter_item(item: Item) -> Item {
    let info = item.endpoint_info().clone();
    let id = info.endpoint_id;
    let mut data = info.data;

    let kept: Vec<iroh::TransportAddr> = data
        .addrs()
        .filter(|a| crate::daemon::dial::is_dialable_addr(a))
        .cloned()
        .collect();
    // `clear_ip_addrs` retains relays, so this removes only the IP entries and re-adds the ones
    // that survived — preserving both the relay entries and the relative order of what is kept.
    data.clear_ip_addrs();
    data.add_addrs(
        kept.into_iter()
            .filter(|a| matches!(a, iroh::TransportAddr::Ip(_))),
    );

    Item::new(
        EndpointInfo::from_parts(id, data),
        item.provenance(),
        item.last_updated(),
    )
}

#[cfg(test)]
mod tests {
    use super::filter_item;
    use iroh::address_lookup::{EndpointData, EndpointInfo, Item};

    /// Build an item WITHOUT going through `EndpointAddr`.
    ///
    /// `From<EndpointAddr> for EndpointData` hardcodes `user_data: None` and `EndpointAddr.addrs`
    /// is a `BTreeSet`, so a fixture built that way can carry neither user_data nor a chosen order
    /// — it cannot express the two things `filter_item` must preserve. The first version used it
    /// and the user_data assertion measured nothing.
    fn item_of(addrs: Vec<iroh::TransportAddr>) -> (iroh::EndpointId, Item) {
        let id = iroh::SecretKey::from_bytes(&[77u8; 32]).public();
        let mut data = EndpointData::new(addrs);
        data.set_user_data(Some("hello".parse().expect("valid user data")));
        (
            id,
            Item::new(EndpointInfo::from_parts(id, data), "test", Some(42)),
        )
    }

    /// A stand-in service that yields one hostile item, so `resolve` itself can be driven.
    #[derive(Debug)]
    struct Fake(Vec<iroh::TransportAddr>);

    impl iroh::address_lookup::AddressLookup for Fake {
        fn resolve(
            &self,
            endpoint_id: iroh::EndpointId,
        ) -> Option<n0_future::boxed::BoxStream<Result<Item, iroh::address_lookup::Error>>>
        {
            let info =
                EndpointInfo::from(iroh::EndpointAddr::from_parts(endpoint_id, self.0.clone()));
            let item = Item::new(info, "fake", None);
            // An Err BETWEEN two Ok items: a `resolve` that filtered only the head, or that
            // swallowed errors with `filter_map(Result::ok)`, would pass a single-Ok fixture.
            // iroh surfaces inline errors only when nothing was emitted, so dropping them is
            // silent.
            Some(Box::pin(n0_future::stream::iter(vec![
                Ok(item.clone()),
                Err(iroh::address_lookup::Error::from_err(
                    "fake",
                    std::io::Error::other("boom"),
                )),
                Ok(item),
            ])))
        }
    }

    /// #203, AT THE CALL SITE: the wrapper's `resolve` actually applies the filter.
    ///
    /// `filter_item` passing says nothing about whether `resolve` calls it — and it did not, in the
    /// first cut: replacing the whole body with `self.0.resolve(endpoint_id)` left both unit tests
    /// green. Fourth occurrence of that failure in this session, so the stream is driven here
    /// rather than the helper asserted.
    #[tokio::test]
    async fn the_wrapper_filters_what_it_yields() {
        use n0_future::StreamExt as _;

        let inner = Fake(vec![
            iroh::TransportAddr::Ip("0.0.0.0:53".parse().unwrap()),
            iroh::TransportAddr::Ip("224.0.0.1:1900".parse().unwrap()),
            iroh::TransportAddr::Ip("192.168.4.4:4433".parse().unwrap()),
        ]);
        let wrapped = super::HygienicLookup(inner);
        let id = iroh::SecretKey::from_bytes(&[78u8; 32]).public();

        let mut stream = iroh::address_lookup::AddressLookup::resolve(&wrapped, id)
            .expect("the wrapper delegates and yields a stream");
        let mut seen = Vec::new();
        while let Some(r) = stream.next().await {
            seen.push(r);
        }
        assert_eq!(seen.len(), 3, "every item is forwarded, errors included");
        assert!(seen[1].is_err(), "an inline error passes through unmapped");
        for (i, r) in [(0usize, &seen[0]), (2, &seen[2])] {
            let it = r.as_ref().unwrap_or_else(|_| panic!("item {i} ok"));
            let addr = iroh::EndpointAddr::from(it.endpoint_info().clone());
            assert_eq!(
                addr.addrs.len(),
                1,
                "EVERY yielded item is filtered, not just the first: {addr:?}"
            );
            assert_eq!(addr.id, id);
        }
    }

    /// #203: a RESOLVED address that can never be a unicast QUIC peer never reaches the path set.
    ///
    /// This is the layer the four call-site filters could not reach. `connect` and `blobs.fetch`
    /// resolve by ID, address lookup runs regardless of what any blob carried, and a pkarr record
    /// is self-signed by the endpoint key — so without this, anyone who generates a key publishes
    /// an arbitrary destination list and iroh dials all of it.
    #[test]
    fn a_resolved_address_that_cannot_be_a_peer_is_stripped() {
        let (id, item) = item_of(vec![
            iroh::TransportAddr::Ip("0.0.0.0:53".parse().unwrap()),
            iroh::TransportAddr::Ip("224.0.0.1:1900".parse().unwrap()),
            iroh::TransportAddr::Ip("255.255.255.255:80".parse().unwrap()),
            // The IPv4-MAPPED forms too — `Ipv6Addr::is_multicast` does not see through them, and
            // iroh canonicalizes on ingest, so an unfiltered mapped address becomes real multicast.
            iroh::TransportAddr::Ip("[::ffff:224.0.0.1]:1900".parse().unwrap()),
            iroh::TransportAddr::Ip("192.168.4.4:4433".parse().unwrap()),
        ]);

        let out = filter_item(item);
        let addr = iroh::EndpointAddr::from(out.endpoint_info().clone());
        assert_eq!(
            addr.addrs.len(),
            1,
            "only the dialable address survives resolution: {addr:?}"
        );
        assert_eq!(
            addr.id, id,
            "and the item still names the endpoint it resolved"
        );
    }

    /// Legitimate resolution is untouched, and the item's metadata survives.
    ///
    /// The failure mode of an ingress filter is not "lets something through" — it is breaking
    /// discovery for everyone. A relay URL in particular MUST survive: it is how a peer behind a
    /// NAT is reached, and dropping it here would silently disable relay-mediated connectivity for
    /// every peer resolved through pkarr or DNS.
    #[test]
    fn legitimate_resolution_and_metadata_survive() {
        let (id, item) = item_of(vec![
            iroh::TransportAddr::Relay("https://relay.example".parse().unwrap()),
            iroh::TransportAddr::Ip("192.168.1.5:4433".parse().unwrap()),
            iroh::TransportAddr::Ip("[2001:db8::1]:4433".parse().unwrap()),
            iroh::TransportAddr::Ip("[fe80::1]:4433".parse().unwrap()),
            iroh::TransportAddr::Ip("127.0.0.1:4433".parse().unwrap()),
        ]);

        let out = filter_item(item);
        assert_eq!(
            out.provenance(),
            "test",
            "the source label must survive — iroh uses it to attribute paths"
        );
        assert_eq!(out.last_updated(), Some(42));
        assert_eq!(
            out.endpoint_info()
                .data
                .user_data()
                .map(ToString::to_string),
            Some("hello".to_string()),
            "user_data is the endpoint's own annotation and nothing to do with reachability — \
             dropping it silently changes behaviour for anyone who reads it"
        );
        // ORDER is preserved: iroh-dns documents it as encoding priority for services that cannot
        // fit every address in one packet.
        let ordered: Vec<String> = out
            .endpoint_info()
            .data
            .addrs()
            .map(ToString::to_string)
            .collect();
        assert!(
            ordered.iter().position(|a| a.contains("192.168.1.5"))
                < ordered.iter().position(|a| a.contains("2001:db8")),
            "the surviving addresses keep their relative order, not a re-sorted one: {ordered:?}"
        );
        let addr = iroh::EndpointAddr::from(out.endpoint_info().clone());
        assert_eq!(addr.id, id);
        assert_eq!(
            addr.addrs.len(),
            5,
            "every legitimate class survives, the relay URL above all: {addr:?}"
        );
        assert!(
            addr.addrs
                .iter()
                .any(|a| matches!(a, iroh::TransportAddr::Relay(_))),
            "dropping the relay would disable relay-mediated connectivity for every resolved \
             peer — the failure mode of an ingress filter is breaking discovery, not leaking: \
             {addr:?}"
        );
    }
}
