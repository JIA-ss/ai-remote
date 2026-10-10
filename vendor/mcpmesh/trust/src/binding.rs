//! Self-sovereign device→user binding for PAIRING mode (no org roster).
//!
//! Roster mode already gives a peer a first-class `user_id` (an org root signs a roster of
//! devices→user_id+groups). Pairing mode did not — every device was just a nickname. This module
//! closes that gap WITHOUT an org: a peer proves its endpoint belongs to a self-minted [`UserKey`]
//! by presenting a device→user binding at pairing, so multiple devices sharing one `UserKey` resolve
//! to the SAME `user_id` and kb audiences can key on the user rather than the per-device nickname.
//!
//! The idea is adopted from an earlier internal device-cert design; the crypto is mcpmesh's OWN — this is a thin
//! wrapper over [`crate::roster::sign::sign_device_binding`]/[`verify_device_binding`] (domain
//! `mcpmesh/join/device-binding/1`) and [`crate::roster::encode_b64u`], not a second cert
//! implementation.
use crate::keys::UserKey;
use crate::roster::RosterError;
use crate::roster::sign::{sign_device_binding, verify_device_binding};
use crate::roster::{decode_b64u, encode_b64u};

/// This user's self-sovereign `user_id`: `encode_b64u(user_pk)`. Stable across device re-keying —
/// the endpoint key can rotate, the user key (and id) does not — and an opaque audience id for
/// consumers (kb `effective_audiences`).
pub fn user_id(user_key: &UserKey) -> String {
    encode_b64u(&user_key.public_bytes())
}

/// Sign THIS device's binding to `user_key` for presentation at pairing. `device_endpoint_id` is
/// this device's own endpoint id. Returns `(user_pk_b64u, binding_sig_b64u)` for the wire.
pub fn present(user_key: &UserKey, device_endpoint_id: &[u8; 32]) -> (String, String) {
    let sig = sign_device_binding(user_key.signing_key(), device_endpoint_id);
    (encode_b64u(&user_key.public_bytes()), encode_b64u(&sig))
}

/// Verify a peer's PRESENTED binding, BOUND to the authenticated transport id. The two invariants:
/// (1) the signature chains to the presented `user_pk`, and (2) it binds THAT device to the
/// TLS-authenticated `authenticated_endpoint` (never a self-asserted id — a transplanted binding for
/// a different endpoint fails). Returns the peer's `user_id` (`encode_b64u(user_pk)`) on success.
pub fn verify_presented(
    user_pk_b64u: &str,
    binding_sig_b64u: &str,
    authenticated_endpoint: &[u8; 32],
) -> Result<String, RosterError> {
    let user_pk: [u8; 32] = decode_b64u(user_pk_b64u)?
        .as_slice()
        .try_into()
        .map_err(|_| RosterError::BadSignature)?;
    let sig = decode_b64u(binding_sig_b64u)?;
    verify_device_binding(&user_pk, authenticated_endpoint, &sig)?;
    Ok(encode_b64u(&user_pk))
}

/// Sign an introduction of `subject` (#65): "I, the holder of this user key, vouch that this
/// endpoint id is that peer's."
///
/// Domain-separated from [`present`]: a device binding says "this endpoint is MINE", an
/// introduction says "this endpoint is SOMEONE ELSE'S". Without separation a binding C made for its
/// own device would verify as C endorsing that device to anyone.
pub fn endorse(
    user_key: &UserKey,
    subject_endpoint_id: &[u8; 32],
    subject_user_pk_b64u: Option<&str>,
) -> Result<String, RosterError> {
    let subject_pk = subject_user_pk_b64u.map(decode32).transpose()?;
    let sig = crate::roster::sign::sign_introduction(
        user_key.signing_key(),
        subject_endpoint_id,
        subject_pk.as_ref(),
    );
    Ok(encode_b64u(&sig))
}

/// Verify an introduction (#65). `endorser_pk_b64u` MUST be a key the caller already trusts — the
/// daemon requires it to be a currently-paired peer's `user_id`, which is what terminates the trust
/// chain at someone the operator paired with themselves.
pub fn verify_endorsement(
    endorser_pk_b64u: &str,
    evidence_b64u: &str,
    subject_endpoint_id: &[u8; 32],
    subject_user_pk_b64u: Option<&str>,
) -> Result<(), RosterError> {
    let endorser_pk = decode32(endorser_pk_b64u)?;
    let subject_pk = subject_user_pk_b64u.map(decode32).transpose()?;
    let sig = decode_b64u(evidence_b64u)?;
    crate::roster::sign::verify_introduction(
        &endorser_pk,
        subject_endpoint_id,
        subject_pk.as_ref(),
        &sig,
    )
}

/// `b64u` → a 32-byte key, refusing anything else rather than truncating or padding.
fn decode32(b64u: &str) -> Result<[u8; 32], RosterError> {
    decode_b64u(b64u)?
        .as_slice()
        .try_into()
        .map_err(|_| RosterError::BadSignature)
}

#[cfg(test)]
mod tests {

    /// #65: an endorsement verifies only for the exact (endorser, subject) pair it was signed for.
    #[test]
    fn an_endorsement_binds_the_endorser_and_the_subject() {
        let carol = user_key();
        let carol_pk = encode_b64u(&carol.public_bytes());
        let bob_eid = [0xBB; 32];

        let ev = endorse(&carol, &bob_eid, None).unwrap();
        verify_endorsement(&carol_pk, &ev, &bob_eid, None).expect("the real pair verifies");

        // A DIFFERENT subject: no transplanting an endorsement onto another endpoint.
        assert!(
            verify_endorsement(&carol_pk, &ev, &[0xCC; 32], None).is_err(),
            "an endorsement must not verify for a subject it does not name"
        );
        // A DIFFERENT endorser: the endorser's own key is in the preimage, so a signature cannot
        // be lifted onto someone else's identity by supplying a mismatched `endorsed_by`.
        let mallory = user_key();
        assert!(
            verify_endorsement(&encode_b64u(&mallory.public_bytes()), &ev, &bob_eid, None).is_err(),
            "an endorsement must not verify under an endorser who did not sign it"
        );
        // The subject's user key is covered too: vouching for a user_id is part of the statement.
        let bob_user = user_key();
        let bob_user_pk = encode_b64u(&bob_user.public_bytes());
        assert!(
            verify_endorsement(&carol_pk, &ev, &bob_eid, Some(&bob_user_pk)).is_err(),
            "an endorsement signed WITHOUT a subject user key must not verify WITH one"
        );
        let ev_with = endorse(&carol, &bob_eid, Some(&bob_user_pk)).unwrap();
        verify_endorsement(&carol_pk, &ev_with, &bob_eid, Some(&bob_user_pk)).unwrap();
        assert!(
            verify_endorsement(&carol_pk, &ev_with, &bob_eid, None).is_err(),
            "…and the reverse"
        );
    }

    /// #65: THE domain-separation property. A device binding says "this endpoint is MINE"; an
    /// introduction says "this endpoint is SOMEONE ELSE'S". Both are signed by a `UserKey`, so
    /// without separation C's binding for its OWN device would verify as C endorsing that device
    /// to anyone — turning every paired peer into an unwitting introducer of itself.
    #[test]
    fn a_device_binding_is_not_an_endorsement_and_vice_versa() {
        let carol = user_key();
        let carol_pk = encode_b64u(&carol.public_bytes());
        let eid = [0xDD; 32];

        let (binding_pk, binding_sig) = present(&carol, &eid);
        assert!(
            verify_endorsement(&binding_pk, &binding_sig, &eid, None).is_err(),
            "a device BINDING must not verify as an introduction — otherwise every peer that ever \
             presented one has silently endorsed its own endpoint to everybody"
        );

        let ev = endorse(&carol, &eid, None).unwrap();
        assert!(
            verify_presented(&carol_pk, &ev, &eid).is_err(),
            "…and an introduction must not verify as a device binding"
        );
    }

    /// A malformed key or signature must ERROR, never panic — this parses caller-supplied b64u.
    #[test]
    fn malformed_endorsement_inputs_error_rather_than_panic() {
        let carol = user_key();
        let ev = endorse(&carol, &[1u8; 32], None).unwrap();
        let pk = encode_b64u(&carol.public_bytes());
        for bad_pk in ["", "!!!!", "AAAA"] {
            assert!(verify_endorsement(bad_pk, &ev, &[1u8; 32], None).is_err());
        }
        for bad_sig in ["", "!!!!", "AAAA"] {
            assert!(verify_endorsement(&pk, bad_sig, &[1u8; 32], None).is_err());
        }
        assert!(endorse(&carol, &[1u8; 32], Some("not-b64u!!")).is_err());
    }
    use super::*;

    fn user_key() -> UserKey {
        let dir = tempfile::tempdir().unwrap();
        UserKey::load_or_generate(&dir.path().join("user.key"))
            .unwrap()
            .0
    }

    #[test]
    fn user_id_is_stable_b64u_of_the_pubkey() {
        let uk = user_key();
        assert_eq!(user_id(&uk), encode_b64u(&uk.public_bytes()));
        assert_eq!(user_id(&uk), user_id(&uk)); // deterministic
    }

    #[test]
    fn present_then_verify_round_trips_bound_to_the_device() {
        let uk = user_key();
        let device = [7u8; 32];
        let (upk, sig) = present(&uk, &device);
        // Verifying against the SAME (authenticated) endpoint yields the user's id.
        assert_eq!(verify_presented(&upk, &sig, &device).unwrap(), user_id(&uk));
    }

    #[test]
    fn a_binding_for_one_device_does_not_verify_for_another() {
        // Invariant 2: the binding must bind to the AUTHENTICATED transport id. A binding minted for
        // device A, replayed by device B, fails — no self-asserted endpoint.
        let uk = user_key();
        let (upk, sig) = present(&uk, &[7u8; 32]);
        assert!(verify_presented(&upk, &sig, &[9u8; 32]).is_err());
    }

    #[test]
    fn a_forged_user_pk_fails() {
        // Invariant 1: the signature must chain to the PRESENTED user_pk. Swapping in a different
        // user_pk (that didn't sign) fails.
        let uk = user_key();
        let (_upk, sig) = present(&uk, &[7u8; 32]);
        let other = encode_b64u(&[3u8; 32]);
        assert!(verify_presented(&other, &sig, &[7u8; 32]).is_err());
    }

    #[test]
    fn malformed_inputs_error_not_panic() {
        assert!(verify_presented("not b64u!!", "also bad", &[7u8; 32]).is_err());
    }
}
