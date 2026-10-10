//! JCS canonicalization (RFC 8785) + Ed25519 org-root sign/verify (validation
//! rule 1). Signature-critical: canonicalize the doc with `sig` REMOVED, sign/verify over those
//! bytes. `sign`/`mint_signed` are production API — `org approve` signs the same way; tests
//! share the same mint path.
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

use super::{Roster, RosterError, decode_b64u, encode_b64u};

/// Canonical (RFC 8785 JCS) bytes of `value` with any top-level `"sig"` key removed. THE signing
/// input. Removing `sig` before canonicalization is what makes the signature cover everything-but-
/// itself. `serde_jcs::to_vec` is the pinned canonicalizer.
pub fn canonical_bytes_without_sig(value: &serde_json::Value) -> Result<Vec<u8>, RosterError> {
    let mut v = value.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("sig");
    }
    serde_jcs::to_vec(&v).map_err(|e| RosterError::Jcs(e.to_string()))
}

/// The signing input for a `Roster` (serialize → JCS-without-sig).
pub fn canonical_bytes(roster: &Roster) -> Result<Vec<u8>, RosterError> {
    canonical_bytes_without_sig(&serde_json::to_value(roster)?)
}

/// Rule 1: verify `roster.sig` (Ed25519, `b64u:`) against `root_pk` over the canonical form.
/// `verify_strict` rejects the malleable/degenerate edge cases dalek documents.
pub fn verify(roster: &Roster, root_pk: &VerifyingKey) -> Result<(), RosterError> {
    let canon = canonical_bytes(roster)?;
    let sig_bytes = decode_b64u(&roster.sig)?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| RosterError::BadSignature)?;
    root_pk
        .verify_strict(&canon, &sig)
        .map_err(|_| RosterError::BadSignature)
}

/// Sign `roster` in place with the org root: canonicalize (with the existing `sig` ignored/removed),
/// sign, set `roster.sig = b64u:<signature>`. Production API (operator-side `org approve`).
pub fn sign(root: &SigningKey, roster: &mut Roster) -> Result<(), RosterError> {
    use ed25519_dalek::Signer;
    let canon = canonical_bytes(roster)?;
    let sig = root.sign(&canon);
    roster.sig = encode_b64u(&sig.to_bytes());
    Ok(())
}

/// Convenience: sign a fresh roster body and return it (the shared mint helper).
pub fn mint_signed(root: &SigningKey, mut body: Roster) -> Roster {
    sign(root, &mut body).expect("mint signs a well-formed body");
    body
}

/// Domain string for the join-code device→user-key binding. DISTINCT from the roster
/// `sig` and the SAS/fingerprint domains, so a signature can never be replayed across purposes.
const DEVICE_BINDING_DOMAIN: &[u8] = b"mcpmesh/join/device-binding/1";

/// Domain for a peer INTRODUCTION (#65): C vouching for B's endpoint to A.
///
/// Separate from [`DEVICE_BINDING_DOMAIN`] so an endorsement can never be replayed as a binding or
/// the reverse — a device binding says "this endpoint is MINE", an introduction says "this endpoint
/// is SOMEONE ELSE'S and I vouch for it". Signed by the same `UserKey`, so without separation a
/// binding C made for its own device would verify as C endorsing that device to anyone.
const INTRODUCE_DOMAIN: &[u8] = b"mcpmesh/introduce/1";

/// The bytes an endorser's user key signs to introduce a subject:
/// domain ∥ endorser_pk ∥ subject_endpoint_id ∥ subject_user_pk (32 zero bytes when absent).
///
/// The endorser's own key is in the preimage as **defence in depth**, not as the primary binding —
/// `verify_strict` already binds the statement to that key, because the key IS the verifier. (An
/// earlier draft of the spec claimed the preimage was what stopped a signature being lifted onto
/// another identity; that was overstated, and removing the field symmetrically from sign+verify
/// escaped the whole suite. The golden vector below is what actually pins this layout.)
fn introduce_preimage(
    endorser_pk: &[u8; 32],
    subject_endpoint_id: &[u8; 32],
    subject_user_pk: Option<&[u8; 32]>,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(INTRODUCE_DOMAIN.len() + 96);
    m.extend_from_slice(INTRODUCE_DOMAIN);
    m.extend_from_slice(endorser_pk);
    m.extend_from_slice(subject_endpoint_id);
    // A FIXED-WIDTH slot rather than an omitted field: a variable-length preimage would let
    // "subject_user_pk absent" and some other framing collide.
    m.extend_from_slice(subject_user_pk.unwrap_or(&[0u8; 32]));
    m
}

/// Sign an introduction (#65) with the endorser's user key.
pub fn sign_introduction(
    endorser_key: &SigningKey,
    subject_endpoint_id: &[u8; 32],
    subject_user_pk: Option<&[u8; 32]>,
) -> [u8; 64] {
    use ed25519_dalek::Signer;
    let endorser_pk = endorser_key.verifying_key().to_bytes();
    let msg = introduce_preimage(&endorser_pk, subject_endpoint_id, subject_user_pk);
    endorser_key.sign(&msg).to_bytes()
}

/// Verify an introduction (#65). `verify_strict`, matching the roster path. Never panics on a
/// malformed key or signature.
pub fn verify_introduction(
    endorser_pk: &[u8; 32],
    subject_endpoint_id: &[u8; 32],
    subject_user_pk: Option<&[u8; 32]>,
    sig: &[u8],
) -> Result<(), RosterError> {
    let vk = ed25519_dalek::VerifyingKey::from_bytes(endorser_pk)
        .map_err(|_| RosterError::BadSignature)?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| RosterError::BadSignature)?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig);
    let msg = introduce_preimage(endorser_pk, subject_endpoint_id, subject_user_pk);
    vk.verify_strict(&msg, &sig)
        .map_err(|_| RosterError::BadSignature)
}

/// The domain the CURRENT org root signs its SUCCESSOR under (#93 ask c).
///
/// Separate from the roster body signature and from every other domain here, so a rotation
/// statement can never be replayed as a roster, a device binding, an introduction, or a device
/// revocation — and, more to the point, so a roster body signature can never be replayed as a
/// statement that moves an org's trust anchor.
const ORG_ROTATION_DOMAIN: &[u8] = b"mcpmesh/org-root-rotation/1";

/// The bytes the current org root signs to name its successor:
/// `domain ∥ len(org_id) as u32 BE ∥ org_id ∥ successor_pk`.
///
/// `org_id` is INSIDE the signature so a rotation statement cannot be replayed into a different org
/// that happens to share an operator. Length-prefixed rather than concatenated raw: `org_id` is
/// variable-length and operator-chosen, so without a prefix `("ab", pk)` and `("a", b"b" ∥ pk)`
/// would produce the same preimage — a confusion an operator could be walked into by naming an org
/// carefully.
fn org_rotation_preimage(org_id: &str, successor_pk: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(ORG_ROTATION_DOMAIN.len() + 4 + org_id.len() + 32);
    m.extend_from_slice(ORG_ROTATION_DOMAIN);
    m.extend_from_slice(&(org_id.len() as u32).to_be_bytes());
    m.extend_from_slice(org_id.as_bytes());
    m.extend_from_slice(successor_pk);
    m
}

/// Sign a successor org root with the CURRENT root (#93 ask c).
pub fn sign_org_rotation(
    current_root: &SigningKey,
    org_id: &str,
    successor_pk: &[u8; 32],
) -> [u8; 64] {
    use ed25519_dalek::Signer;
    current_root
        .sign(&org_rotation_preimage(org_id, successor_pk))
        .to_bytes()
}

/// Verify a successor statement against the anchor a node CURRENTLY pins.
///
/// The whole security of rotation is that this is checked with the PINNED key — the one the node
/// already trusts — never with the successor being introduced. Verified with `verify_strict`, like
/// every other signature here.
pub fn verify_org_rotation(
    pinned_root_pk: &[u8; 32],
    org_id: &str,
    successor_pk: &[u8; 32],
    sig: &[u8],
) -> Result<(), RosterError> {
    let vk = VerifyingKey::from_bytes(pinned_root_pk).map_err(|_| RosterError::BadSignature)?;
    let sig = Signature::from_slice(sig).map_err(|_| RosterError::BadSignature)?;
    vk.verify_strict(&org_rotation_preimage(org_id, successor_pk), &sig)
        .map_err(|_| RosterError::BadSignature)
}

/// The domain a user key signs a REVOCATION of one of its own devices under (#85 ask 4).
///
/// Separate from [`DEVICE_BINDING_DOMAIN`] for a sharper reason than the others: a device binding
/// signs `domain ∥ user_pk ∥ endpoint_id` — the **identical field layout** with the identical
/// values. Without a distinct domain, every binding a person has ever issued would verify as a
/// revocation of that same device, so presenting your own binding would kill you. Not hypothetical;
/// the preimages differ in nothing else but the prefix and the timestamp.
const DEVICE_REVOCATION_DOMAIN: &[u8] = b"mcpmesh/device-revocation/1";

/// The bytes a user key signs to revoke one of its own devices:
/// `domain ∥ user_pk ∥ endpoint_id ∥ issued_at(8, big-endian)`.
///
/// `issued_at` is INSIDE the signature so a fresher statement about the same endpoint cannot be
/// replaced by replaying an older one, and so an importer can order two statements it receives out
/// of order. Big-endian and fixed-width, like the other preimages here: the layout is wire format.
fn device_revocation_preimage(
    user_pk: &[u8; 32],
    device_endpoint_id: &[u8; 32],
    issued_at: u64,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(DEVICE_REVOCATION_DOMAIN.len() + 72);
    m.extend_from_slice(DEVICE_REVOCATION_DOMAIN);
    m.extend_from_slice(user_pk);
    m.extend_from_slice(device_endpoint_id);
    m.extend_from_slice(&issued_at.to_be_bytes());
    m
}

/// Sign a revocation of `device_endpoint_id` with the USER key (#85 ask 4).
///
/// Says "the holder of this user key asks that this endpoint be treated as dead". It does NOT prove
/// the endpoint was ever bound to that key — the importer enforces that separately, against rows it
/// already holds. See `daemon::handlers::device_revocation_import`.
pub fn sign_device_revocation(
    user_key: &SigningKey,
    device_endpoint_id: &[u8; 32],
    issued_at: u64,
) -> [u8; 64] {
    use ed25519_dalek::Signer;
    let user_pk = user_key.verifying_key().to_bytes();
    let msg = device_revocation_preimage(&user_pk, device_endpoint_id, issued_at);
    user_key.sign(&msg).to_bytes()
}

/// Verify a device revocation. `verify_strict`, like every other verifier here. Never panics on a
/// malformed key or signature.
pub fn verify_device_revocation(
    user_pk: &[u8; 32],
    device_endpoint_id: &[u8; 32],
    issued_at: u64,
    sig: &[u8],
) -> Result<(), RosterError> {
    let vk = VerifyingKey::from_bytes(user_pk).map_err(|_| RosterError::BadSignature)?;
    let sig = Signature::from_slice(sig).map_err(|_| RosterError::BadSignature)?;
    let msg = device_revocation_preimage(user_pk, device_endpoint_id, issued_at);
    vk.verify_strict(&msg, &sig)
        .map_err(|_| RosterError::BadSignature)
}

/// The bytes a user key signs to bind a device endpoint to itself: domain ∥ user_pk ∥ endpoint_id.
fn device_binding_preimage(user_pk: &[u8; 32], device_endpoint_id: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(DEVICE_BINDING_DOMAIN.len() + 64);
    m.extend_from_slice(DEVICE_BINDING_DOMAIN);
    m.extend_from_slice(user_pk);
    m.extend_from_slice(device_endpoint_id);
    m
}

/// Sign a device→user-key binding with the USER key (the join code's `binding_sig`).
/// Proves `device_endpoint_id` belongs to the holder of `user_key` WITHOUT trusting the transport —
/// the human ceremony verifies the PERSON, this verifies the DEVICE. Returns raw 64-byte signature.
pub fn sign_device_binding(user_key: &SigningKey, device_endpoint_id: &[u8; 32]) -> [u8; 64] {
    use ed25519_dalek::Signer;
    let user_pk = user_key.verifying_key().to_bytes();
    let msg = device_binding_preimage(&user_pk, device_endpoint_id);
    user_key.sign(&msg).to_bytes()
}

/// Verify a device→user-key binding (`org approve`). `user_pk` + `device_endpoint_id`
/// come from the join code; `sig` is its `binding_sig`. `verify_strict` (conservative, matches the
/// roster `verify`). `Ok(())` iff the binding holds. Never panics on a malformed key/sig.
pub fn verify_device_binding(
    user_pk: &[u8; 32],
    device_endpoint_id: &[u8; 32],
    sig: &[u8],
) -> Result<(), RosterError> {
    let vk = VerifyingKey::from_bytes(user_pk).map_err(|_| RosterError::BadSignature)?;
    let sig = Signature::from_slice(sig).map_err(|_| RosterError::BadSignature)?;
    let msg = device_binding_preimage(user_pk, device_endpoint_id);
    vk.verify_strict(&msg, &sig)
        .map_err(|_| RosterError::BadSignature)
}

// A small body builder shared by the tests (a valid, unsigned-ready roster body).
#[cfg(test)]
fn sample_body() -> crate::roster::Roster {
    use crate::roster::{Roster, RosterDevice, RosterUser, encode_b64u};
    Roster {
        format: "mcpmesh-roster/1".into(),
        org_id: "acme".into(),
        serial: 1,
        issued_at: "2026-07-03T12:00:00Z".into(),
        expires_at: "2026-10-01T00:00:00Z".into(),
        groups: vec!["team-eng".into(), "all".into()],
        users: vec![RosterUser {
            user_id: "alice".into(),
            display_name: "Alice".into(),
            user_pk: encode_b64u(&[1u8; 32]),
            groups: vec!["team-eng".into(), "all".into()],
            devices: vec![RosterDevice {
                endpoint_id: encode_b64u(&[2u8; 32]),
                label: "laptop".into(),
                role: "primary".into(),
            }],
        }],
        revoked_endpoints: vec![],
        successor_root_pk: None,
        successor_sig: None,
        sig: String::new(),
    }
}

#[cfg(test)]
mod tests {
    /// #65: a GOLDEN VECTOR for the introduction preimage.
    ///
    /// The domain string and the field layout are wire format: a peer signing with one layout and a
    /// peer verifying with another simply cannot interoperate, and a domain change silently makes
    /// every existing endorsement unverifiable. Round-trip tests cannot catch either, because
    /// sign and verify share the same function — changing it symmetrically keeps them agreeing
    /// with each other and disagreeing with every other build. Two such mutations escaped the
    /// whole suite before this test existed.
    #[test]
    fn the_introduction_preimage_layout_is_pinned() {
        let endorser = [1u8; 32];
        let subject = [2u8; 32];
        let subject_user = [3u8; 32];

        let without = super::introduce_preimage(&endorser, &subject, None);
        assert_eq!(
            &without[..super::INTRODUCE_DOMAIN.len()],
            b"mcpmesh/introduce/1",
            "the DOMAIN is wire format — changing it invalidates every endorsement in existence"
        );
        assert_eq!(
            without.len(),
            super::INTRODUCE_DOMAIN.len() + 96,
            "domain ∥ endorser_pk ∥ subject ∥ subject_user_pk(32 zero bytes) — a FIXED width, so \
             'absent' cannot collide with some other framing"
        );
        assert_eq!(&without[super::INTRODUCE_DOMAIN.len()..][..32], &endorser);
        assert_eq!(
            &without[super::INTRODUCE_DOMAIN.len() + 32..][..32],
            &subject
        );
        assert_eq!(
            &without[super::INTRODUCE_DOMAIN.len() + 64..][..32],
            &[0u8; 32],
            "the absent subject user key is 32 ZERO bytes, not an omission"
        );

        let with = super::introduce_preimage(&endorser, &subject, Some(&subject_user));
        assert_eq!(
            &with[super::INTRODUCE_DOMAIN.len() + 64..][..32],
            &subject_user
        );
        assert_ne!(
            without, with,
            "present and absent must not produce one preimage"
        );

        // And the two domains are distinct — the separation the device-binding test relies on.
        assert_ne!(super::INTRODUCE_DOMAIN, super::DEVICE_BINDING_DOMAIN);
    }

    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn sign_then_verify_round_trips_and_a_tamper_fails() {
        let root = SigningKey::from_bytes(&[9u8; 32]);
        let mut roster = mint_signed(&root, sample_body());
        // A freshly signed roster verifies against the exact pinned root.
        verify(&roster, &root.verifying_key()).expect("valid signature verifies");
        // A wrong root rejects.
        let other = SigningKey::from_bytes(&[8u8; 32]);
        assert!(matches!(
            verify(&roster, &other.verifying_key()),
            Err(RosterError::BadSignature)
        ));
        // Any content tamper (bump serial) invalidates the signature.
        roster.serial += 1;
        assert!(matches!(
            verify(&roster, &root.verifying_key()),
            Err(RosterError::BadSignature)
        ));
    }

    #[test]
    fn canonicalization_is_key_order_independent() {
        // JCS sorts keys, so two Values with the same content in different key orders produce
        // identical canonical bytes → identical signatures (the whole point of RFC 8785).
        let a = serde_json::json!({ "b": 1, "a": 2, "sig": "b64u:ZZZZ" });
        let b = serde_json::json!({ "a": 2, "sig": "b64u:YYYY", "b": 1 });
        assert_eq!(
            canonical_bytes_without_sig(&a).unwrap(),
            canonical_bytes_without_sig(&b).unwrap()
        );
    }

    #[test]
    fn malformed_or_empty_sig_is_rejected_and_never_panics() {
        // An empty or wrong-length signature is a rejection, never a panic or a bypass. The two
        // cases reject at DIFFERENT pipeline stages: an empty (no `b64u:` prefix) sig fails at
        // decode (`RosterError::Encoding`); a valid-b64u but wrong-length sig fails at
        // `Signature::from_slice`/`verify_strict` (`RosterError::BadSignature`). Both are `is_err`
        // — assert the rejection, not one specific variant (don't over-tighten).
        let root = SigningKey::from_bytes(&[9u8; 32]);
        let mut roster = mint_signed(&root, sample_body());
        roster.sig = String::new(); // no `b64u:` prefix → decode fails first (Encoding)
        assert!(verify(&roster, &root.verifying_key()).is_err());
        roster.sig = "b64u:AAAA".into(); // valid b64u, decodes to 3 bytes ≠ 64 → BadSignature
        assert!(verify(&roster, &root.verifying_key()).is_err());
    }

    #[test]
    fn a_sig_from_another_roster_cannot_be_transplanted() {
        // A valid signature over roster A must not verify when copied onto a DIFFERENT roster B
        // (distinct from the in-place serial+1 tamper): B's canonical bytes differ from what A.sig
        // signed, so `verify_strict` rejects. Pins that a signature is bound to its exact content.
        let root = SigningKey::from_bytes(&[9u8; 32]);
        let a = mint_signed(&root, sample_body());
        let mut b = sample_body(); // different content than A ...
        b.serial = 999;
        b.org_id = "evil-corp".into();
        b.sig = a.sig.clone(); // ... but wearing A's valid signature
        assert!(matches!(
            verify(&b, &root.verifying_key()),
            Err(RosterError::BadSignature)
        ));
    }

    #[test]
    fn device_binding_sign_then_verify_and_forgeries_fail() {
        let user = SigningKey::from_bytes(&[5u8; 32]);
        let user_pk = user.verifying_key().to_bytes();
        let device = [7u8; 32];
        let sig = sign_device_binding(&user, &device);
        // The genuine binding verifies.
        assert!(verify_device_binding(&user_pk, &device, &sig).is_ok());
        // A DIFFERENT device endpoint fails (the whole point: the sig binds THIS endpoint).
        assert!(verify_device_binding(&user_pk, &[8u8; 32], &sig).is_err());
        // A DIFFERENT user_pk fails (a device claimed by the wrong user key).
        let other_pk = SigningKey::from_bytes(&[6u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(verify_device_binding(&other_pk, &device, &sig).is_err());
        // A tampered / malformed sig fails, never panics.
        let mut bad = sig;
        bad[0] ^= 0xFF;
        assert!(verify_device_binding(&user_pk, &device, &bad).is_err());
        assert!(verify_device_binding(&user_pk, &device, b"short").is_err());
    }

    /// #85 ask 4: a device BINDING must never verify as a device REVOCATION, or presenting your own
    /// binding would kill you.
    ///
    /// A binding signs `domain ∥ user_pk ∥ endpoint_id`; a revocation signs
    /// `domain ∥ user_pk ∥ endpoint_id ∥ issued_at`. Same key, same endpoint, same order — the
    /// prefix and the eight trailing bytes are the only things standing between "this device is
    /// mine" and "this device is dead". A binding is a public artifact: it rides the pairing wire,
    /// and every peer who ever paired with this person holds one.
    ///
    /// **Which property is load-bearing, measured rather than assumed:** collapsing the two domains
    /// to one leaves this test GREEN — the trailing `issued_at` still makes the preimages different
    /// lengths. The distinct domain is defence in depth against a future layout change that removes
    /// that accident, and it is `the_revocation_preimage_layout_is_fixed` below that actually
    /// catches a collision. Recorded because calling this the sharpest domain case and then not
    /// testing the domain would be exactly the vacuity the mutation exists to find.
    #[test]
    fn a_device_binding_can_never_be_replayed_as_a_revocation() {
        let uk = SigningKey::from_bytes(&[3u8; 32]);
        let pk = uk.verifying_key().to_bytes();
        let device = [11u8; 32];

        let binding = sign_device_binding(&uk, &device);
        let revocation = sign_device_revocation(&uk, &device, 1_754_300_000);

        // Each verifies as ITSELF — the precondition, without which the failures below prove
        // nothing (two broken signers would also "not cross-verify").
        verify_device_binding(&pk, &device, &binding).expect("a binding verifies as a binding");
        verify_device_revocation(&pk, &device, 1_754_300_000, &revocation)
            .expect("a revocation verifies as a revocation");
        assert_ne!(
            binding, revocation,
            "precondition: the two signatures are actually different"
        );

        // Neither verifies as the OTHER, at any timestamp.
        for t in [0u64, 1_754_300_000, u64::MAX] {
            assert!(
                verify_device_revocation(&pk, &device, t, &binding).is_err(),
                "a device binding must NOT verify as a revocation at issued_at={t} — every peer \
                 this person paired with holds one of these"
            );
        }
        assert!(
            verify_device_binding(&pk, &device, &revocation).is_err(),
            "…and a revocation must not verify as a binding, which would let a kill statement \
             admit the device it names"
        );
    }

    /// The revocation preimage is WIRE FORMAT: domain ∥ user_pk ∥ endpoint ∥ issued_at(8, BE).
    ///
    /// Pinned by layout, not by a golden signature, for the same reason the introduction preimage
    /// is: a peer signing one layout and a peer verifying another simply cannot interoperate, and a
    /// silent change invalidates every revocation in existence — including ones already handed to
    /// peers out of band, which cannot be re-issued from a stolen laptop.
    #[test]
    fn the_revocation_preimage_layout_is_fixed() {
        let pk = [1u8; 32];
        let device = [2u8; 32];
        let m = super::device_revocation_preimage(&pk, &device, 0x0102_0304_0506_0708);
        let d = super::DEVICE_REVOCATION_DOMAIN;
        assert_eq!(&m[..d.len()], d);
        assert_eq!(
            m.len(),
            d.len() + 72,
            "domain ∥ pk(32) ∥ endpoint(32) ∥ issued_at(8)"
        );
        assert_eq!(&m[d.len()..][..32], &pk);
        assert_eq!(&m[d.len() + 32..][..32], &device);
        assert_eq!(
            &m[d.len() + 64..],
            &[1u8, 2, 3, 4, 5, 6, 7, 8],
            "issued_at is big-endian — byte order is wire format too"
        );
        assert_ne!(
            d,
            super::DEVICE_BINDING_DOMAIN,
            "the two domains must differ; the field layouts do not"
        );
    }

    /// `issued_at` is inside the signature, so a revocation cannot be re-dated.
    ///
    /// Without this an attacker who holds a revocation could present it with any timestamp, which
    /// defeats the ordering an importer uses to decide whether a newer statement supersedes it.
    #[test]
    fn a_revocations_timestamp_cannot_be_altered() {
        let uk = SigningKey::from_bytes(&[5u8; 32]);
        let pk = uk.verifying_key().to_bytes();
        let device = [7u8; 32];
        let sig = sign_device_revocation(&uk, &device, 1000);
        verify_device_revocation(&pk, &device, 1000, &sig).expect("verifies at its own timestamp");
        for t in [999u64, 1001, 0] {
            assert!(
                verify_device_revocation(&pk, &device, t, &sig).is_err(),
                "re-dating to {t} must fail — issued_at is signed"
            );
        }
        // …and it is bound to the ENDPOINT, so it cannot be transplanted onto another device.
        assert!(
            verify_device_revocation(&pk, &[8u8; 32], 1000, &sig).is_err(),
            "a revocation must not transplant to a different endpoint"
        );
        // …nor to another signer.
        let other = SigningKey::from_bytes(&[6u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(verify_device_revocation(&other, &device, 1000, &sig).is_err());
    }

    /// #93 ask c: a rotation statement is confusable with nothing else in this tree.
    ///
    /// Every domain here guards a different confusion, and this one guards the sharpest: a
    /// statement that MOVES AN ORG'S TRUST ANCHOR. If a roster body signature, a device binding, an
    /// introduction or a device revocation could be replayed as one, anyone holding an artifact the
    /// org root has ever signed could re-anchor a node.
    #[test]
    fn a_rotation_statement_is_confusable_with_nothing_else() {
        let root = SigningKey::from_bytes(&[3u8; 32]);
        let pk = root.verifying_key().to_bytes();
        let successor = [11u8; 32];

        let rotation = sign_org_rotation(&root, "acme", &successor);
        verify_org_rotation(&pk, "acme", &successor, &rotation).expect("verifies as itself");

        // A device BINDING over the same 32 bytes must not verify as a rotation, and vice versa.
        let binding = sign_device_binding(&root, &successor);
        assert!(
            verify_org_rotation(&pk, "acme", &successor, &binding).is_err(),
            "a device binding must not move an org's trust anchor"
        );
        assert!(
            verify_device_binding(&pk, &successor, &rotation).is_err(),
            "…and a rotation statement must not bind a device"
        );

        // An INTRODUCTION and a device REVOCATION over the same subject, likewise.
        let intro = sign_introduction(&root, &successor, None);
        assert!(verify_org_rotation(&pk, "acme", &successor, &intro).is_err());
        let revocation = sign_device_revocation(&root, &successor, 0);
        assert!(verify_org_rotation(&pk, "acme", &successor, &revocation).is_err());
        assert!(
            verify_device_revocation(&pk, &successor, 0, &rotation).is_err(),
            "…and a rotation must not read as a revocation of the successor it names"
        );

        // The org_id is length-prefixed, so a carefully-chosen pair of org names cannot collide:
        // without the prefix ("ab", pk) and ("a", "b" ∥ pk) share a preimage.
        let a = sign_org_rotation(&root, "ab", &successor);
        assert!(
            verify_org_rotation(&pk, "a", &successor, &a).is_err(),
            "the length prefix must stop an org-name splitting confusion"
        );
    }
}
