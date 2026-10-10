//! Application-payload signing with the DEVICE key (#59).
//!
//! mcpmesh authenticates the *transport*: inside a session, `_meta["mcpmesh/peer"]` tells a service
//! who is calling. That is exactly right for request/response and useless for a payload that
//! outlives its connection. An embedder doing store-and-forward — offline delivery, an always-on
//! relay, a mailbox, an app-level gossip overlay — handles bytes that arrived from someone other
//! than their author, and the transport authenticated the FORWARDER.
//!
//! Attributing the ORIGIN without this meant minting a second identity per device, with its own
//! storage, backup and revocation story, plus a binding protocol proving "endpoint X asserts app
//! key Y" — security-critical code duplicated in every embedder, sitting next to an ed25519 key
//! that already identifies the device.
//!
//! So: sign with the device key, and verify against the `EndpointId` the transport already
//! authenticates. No second identity, no second chain.
//!
//! # Domain separation is enforced here, not left to callers
//!
//! The preimage is `APP_SIG_DOMAIN ∥ len(domain) ∥ domain ∥ msg`. Two properties follow, and both
//! are properties of the API rather than of caller discipline:
//!
//! 1. **An app signature is never an mcpmesh signature.** [`APP_SIG_DOMAIN`] is a fixed prefix a
//!    caller cannot escape, so no caller-chosen `domain` can produce a preimage mcpmesh itself
//!    signs — nor any domain it adds later — and the reverse holds too. Without it, an embedder
//!    that let a peer choose the bytes it signs would be an oracle for forging mcpmesh's own
//!    statements.
//!
//!    Two precisions, because this is easy to over-claim in both directions.
//!
//!    **Which statements are actually at risk.** Only ones signed by the SAME key. A device
//!    binding (`mcpmesh/join/device-binding/1`), an introduction (`mcpmesh/introduce/1`) and a
//!    roster `sig` are signed by a [`UserKey`](crate::UserKey) / [`OrgRootKey`](crate::OrgRootKey),
//!    so they were never reachable from here whatever the domain. The statement this key does sign
//!    is the roster-mode **presence heartbeat** (`mcpmesh/presence/1`) — that is the domain this
//!    prefix has to stay clear of, and the one to check against if either string ever changes.
//!
//!    **Today the length field alone would also separate them**, since every mcpmesh preimage opens
//!    with an ASCII domain string while an app preimage opens with a small little-endian `u64`.
//!    That is an accident of the current domains, not an invariant — a future preimage with a
//!    different shape would erase it silently. The fixed prefix makes the separation structural, so
//!    it is pinned by the LAYOUT test rather than left to hold by coincidence.
//! 2. **Two app domains cannot collide.** The length prefix means `(b"ab", b"c")` and
//!    `(b"a", b"bc")` have different preimages. Plain concatenation would let a signature made for
//!    one domain verify under another, which is the whole failure domain separation exists to
//!    prevent — reintroduced one level down.
//!
//! Note what this does NOT do: it says nothing about whether the signer was *entitled* to make the
//! statement. That is the embedder's authorization question, answered from its own state — this
//! answers only "which device produced these bytes".
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// The fixed prefix every app signature carries (#59). Distinct from every mcpmesh signing domain,
/// and — because it is a prefix a caller cannot influence — the reason an app signature and an
/// mcpmesh signature can never be confused for one another whatever `domain` an embedder picks.
///
/// **Wire format.** Changing this string invalidates every app signature in existence, exactly as
/// changing a roster domain would.
pub const APP_SIG_DOMAIN: &[u8] = b"mcpmesh/app-sig/1";

/// The bytes actually signed: `APP_SIG_DOMAIN ∥ (domain.len() as u64 LE) ∥ domain ∥ msg`.
///
/// The length prefix is load-bearing, not tidiness — see the module doc. It is a FIXED width so
/// the boundary between `domain` and `msg` is unambiguous for any content of either.
fn preimage(domain: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(APP_SIG_DOMAIN.len() + 8 + domain.len() + msg.len());
    m.extend_from_slice(APP_SIG_DOMAIN);
    m.extend_from_slice(&(domain.len() as u64).to_le_bytes());
    m.extend_from_slice(domain);
    m.extend_from_slice(msg);
    m
}

/// Sign `msg` under `domain` with this device's key (#59).
///
/// `domain` is the embedder's namespace for the STATEMENT ("chat/message/1", "mailbox/receipt/1").
/// Pick one per kind of thing signed: a signature is only as narrow as its domain, and reusing one
/// domain for two statement shapes lets a value from one be read as the other.
pub fn sign_app(device_key: &SigningKey, domain: &[u8], msg: &[u8]) -> [u8; 64] {
    device_key.sign(&preimage(domain, msg)).to_bytes()
}

/// Verify an app signature against the device that made it (#59).
///
/// `endpoint_id` is the peer's mesh identity — the same 32 bytes the transport authenticates and
/// that `_meta["mcpmesh/peer"]`'s `eid:` principal names, so a verifier needs nothing it does not
/// already have.
///
/// Returns `false` rather than erroring, and never panics: every input here is attacker-supplied
/// by construction (a relayed payload is exactly the case this exists for), and a malformed key or
/// signature is simply not a valid signature. `verify_strict` rejects the malleable and degenerate
/// edge cases dalek documents, matching the roster verifier.
pub fn verify_app(endpoint_id: &[u8; 32], domain: &[u8], msg: &[u8], sig: &[u8; 64]) -> bool {
    let Ok(pk) = VerifyingKey::from_bytes(endpoint_id) else {
        return false; // not a valid ed25519 point — no signature can verify under it
    };
    let sig = Signature::from_bytes(sig);
    pk.verify_strict(&preimage(domain, msg), &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn eid(k: &SigningKey) -> [u8; 32] {
        k.verifying_key().to_bytes()
    }

    #[test]
    fn a_signature_verifies_for_its_own_device_domain_and_message() {
        let k = key(1);
        let sig = sign_app(&k, b"chat/message/1", b"hello");
        assert!(verify_app(&eid(&k), b"chat/message/1", b"hello", &sig));

        // Each of the three inputs is covered. Dropping any one from the preimage leaves the
        // corresponding assertion below passing when it must not.
        assert!(
            !verify_app(&eid(&k), b"chat/message/1", b"hell0", &sig),
            "the MESSAGE must be covered"
        );
        assert!(
            !verify_app(&eid(&k), b"chat/receipt/1", b"hello", &sig),
            "the DOMAIN must be covered — otherwise a signature made for one statement kind is \
             valid for another"
        );
        assert!(
            !verify_app(&eid(&key(2)), b"chat/message/1", b"hello", &sig),
            "the DEVICE must be covered — attribution is the entire feature"
        );
    }

    /// #59: the length prefix, which is the difference between domain separation and the
    /// appearance of it.
    ///
    /// With plain concatenation `domain ∥ msg`, the pairs below produce identical preimages, so a
    /// signature over ("ab", "c") verifies as one over ("a", "bc"). An embedder that lets a peer
    /// influence either half then gets to choose which statement its signature reads as. Removing
    /// the length prefix flips both assertions.
    #[test]
    fn the_domain_boundary_is_unambiguous() {
        let k = key(3);
        let sig = sign_app(&k, b"ab", b"c");
        assert!(verify_app(&eid(&k), b"ab", b"c", &sig));
        assert!(
            !verify_app(&eid(&k), b"a", b"bc", &sig),
            "a shifted domain/message split must NOT verify — the two are the same bytes \
             concatenated"
        );
        assert!(
            !verify_app(&eid(&k), b"", b"abc", &sig),
            "…including the empty-domain split"
        );
    }

    /// #59, the property the fixed prefix exists for: an app signature and an mcpmesh signature
    /// live in disjoint spaces, whatever domain the embedder chooses.
    ///
    /// This is the same shape as `a_device_binding_is_not_an_endorsement_and_vice_versa`, one layer
    /// out. The dangerous direction is an embedder that signs peer-chosen bytes: a peer picking an
    /// mcpmesh domain and a crafted message must not get the device key to emit a valid mcpmesh
    /// statement — a forged claim, from an API whose whole purpose is to hand embedders a safe use
    /// of that key.
    ///
    /// The FIRST case below is the one that matters, and it took a review to get right: the
    /// statements this key actually signs are what is at risk, and the device key signs the
    /// roster-mode PRESENCE heartbeat. Device bindings and introductions are signed by a `UserKey`,
    /// so they are unreachable from here whatever the domain — kept anyway as a regression guard
    /// in case a future statement moves onto the device key.
    #[test]
    fn an_app_signature_is_never_an_mcpmesh_signature() {
        let k = key(4);
        let device = [7u8; 32];

        // THE case: the presence heartbeat, which this same device key signs
        // (`node/src/roster/presence.rs`, domain `mcpmesh/presence/1`). The two preimages must not
        // meet, so an embedder signing peer-chosen bytes can never emit a presence claim.
        const PRESENCE_DOMAIN: &[u8] = b"mcpmesh/presence/1";
        assert_ne!(
            APP_SIG_DOMAIN, PRESENCE_DOMAIN,
            "the app domain must differ from the one statement the DEVICE key signs"
        );
        let presence_shaped = preimage(PRESENCE_DOMAIN, b"whatever a heartbeat body looks like");
        assert!(
            !presence_shaped.starts_with(PRESENCE_DOMAIN),
            "an app preimage must never open with a presence preimage's bytes — a signature over \
             it would be a valid heartbeat"
        );
        assert!(
            presence_shaped.starts_with(APP_SIG_DOMAIN),
            "…because the fixed app prefix always comes first"
        );

        // And the same argument against a UserKey statement, which is unreachable today but is the
        // shape a future device-key statement would take. The caller names the domain verbatim and
        // supplies the binding's own payload as the message.
        let mut forged_msg = k.verifying_key().to_bytes().to_vec();
        forged_msg.extend_from_slice(&device);
        let sig = sign_app(&k, b"mcpmesh/join/device-binding/1", &forged_msg);
        assert!(
            crate::roster::sign::verify_device_binding(&eid(&k), &device, &sig).is_err(),
            "an app signature must never verify as a device binding, even when the caller picks \
             the binding's domain and payload"
        );

        // And the reverse: a real binding must not verify as an app signature under the domain it
        // was made for.
        let binding = crate::roster::sign::sign_device_binding(&k, &device);
        assert!(
            !verify_app(
                &eid(&k),
                b"mcpmesh/join/device-binding/1",
                &forged_msg,
                &binding
            ),
            "…and an mcpmesh signature must not verify as an app signature"
        );
    }

    /// #59: the preimage LAYOUT, asserted directly — the sibling of sign.rs's
    /// `the_signed_bytes_are_the_documented_layout`.
    ///
    /// Needed because the behavioural tests cannot see this. Deleting `APP_SIG_DOMAIN` from the
    /// preimage leaves every one of them green: the length field happens to separate app preimages
    /// from mcpmesh's current ones on its own, so the fixed prefix's contribution is invisible from
    /// the outside. That is exactly the coincidence the module doc refuses to rely on, so the
    /// prefix is pinned here as WIRE FORMAT instead.
    #[test]
    fn the_signed_bytes_are_the_documented_layout() {
        let m = preimage(b"abc", b"payload");
        assert_eq!(
            &m[..APP_SIG_DOMAIN.len()],
            APP_SIG_DOMAIN,
            "every app preimage must open with the fixed domain — it is wire format, and dropping \
             it invalidates every app signature in existence"
        );
        let rest = &m[APP_SIG_DOMAIN.len()..];
        assert_eq!(
            &rest[..8],
            &3u64.to_le_bytes(),
            "…then the domain length as a FIXED-WIDTH little-endian u64"
        );
        assert_eq!(&rest[8..11], b"abc", "…then the domain");
        assert_eq!(
            &rest[11..],
            b"payload",
            "…then the message, and nothing else"
        );
        assert_eq!(m.len(), APP_SIG_DOMAIN.len() + 8 + 3 + 7);

        // And it is DISTINCT from mcpmesh's own domains, which is the property the prefix buys.
        assert_ne!(APP_SIG_DOMAIN, b"mcpmesh/join/device-binding/1");
        assert_ne!(APP_SIG_DOMAIN, b"mcpmesh/introduce/1");
    }

    /// This parses attacker-supplied bytes — a relayed payload is the whole use case — so every
    /// malformed input must be a `false`, never a panic.
    #[test]
    fn malformed_inputs_are_false_not_a_panic() {
        let k = key(5);
        let sig = sign_app(&k, b"d", b"m");

        // Not a valid ed25519 point. `from_bytes` accepts many byte strings, so this is the
        // decompression failure path rather than a signature mismatch.
        assert!(!verify_app(&[0xFFu8; 32], b"d", b"m", &sig));
        assert!(!verify_app(&[0u8; 32], b"d", b"m", &sig));
        // Garbage signature bytes.
        assert!(!verify_app(&eid(&k), b"d", b"m", &[0u8; 64]));
        assert!(!verify_app(&eid(&k), b"d", b"m", &[0xFFu8; 64]));
        // Empty domain and empty message are legal inputs, not errors.
        let empty = sign_app(&k, b"", b"");
        assert!(verify_app(&eid(&k), b"", b"", &empty));
    }
}
