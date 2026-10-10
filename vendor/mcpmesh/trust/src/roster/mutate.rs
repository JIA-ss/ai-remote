//! Pure roster TRANSFORMS (net-free, redb-free) — the operator-side mutations `org create`/`approve`/
//! `revoke` apply BEFORE signing. Each maintains the schema invariants
//! (flat namespace disjointness, declared groups) so the subsequent [`validate_for_install`] accepts,
//! and CLEARS `sig` (the caller re-signs via [`sign`]). Serial bumping + signing + install are the
//! cli/daemon plumbing; this module is the roster-mutation domain.
//!
//! [`validate_for_install`]: crate::roster::validate::validate_for_install
//! [`sign`]: crate::roster::sign::sign
use super::{ROSTER_FORMAT, Roster, RosterDevice, RosterUser};

/// Typed roster-mutation failure — the pre-sign guard rejections (the `validate_for_install`
/// pre-image checks plus the intended-semantic revoked-endpoint guard) and unknown-target
/// lookups. Parallel to the crate's [`RosterError`](super::RosterError) (validation) but a
/// separate enum: these are OPERATOR-INPUT rejections, not document-validation failures.
/// `Display` wording is the porcelain contract — the cli prints these verbatim.
/// `#[non_exhaustive]` so a future guard is not a breaking change — match with a
/// wildcard arm.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MutateError {
    /// Rule 5a pre-image: the user_id equals an existing (or newly requested) group name.
    #[error("user_id {0:?} collides with a group name (flat namespace must be disjoint)")]
    UserIdIsGroup(String),
    /// Rule 5a pre-image (the other direction): a requested group equals an existing user_id.
    #[error("group {0:?} collides with an existing user_id (flat namespace must be disjoint)")]
    GroupIsUserId(String),
    /// Rule 4a pre-image: the device endpoint is already listed under a different user.
    #[error("device endpoint {0:?} already belongs to another user")]
    EndpointBelongsToAnotherUser(String),
    /// Intended-semantic guard: re-adding a currently-revoked endpoint would make a
    /// silently-dead device (rule 4b, revocation-wins). No un-revoke path by design.
    #[error("device endpoint {0:?} is revoked; the device must re-join with a fresh key")]
    EndpointRevoked(String),
    #[error("no such person {0:?}")]
    NoSuchPerson(String),
    #[error("no such device {user_id}/{device_label}")]
    NoSuchDevice {
        user_id: String,
        device_label: String,
    },
}

/// A fresh EMPTY roster for `org create`: `serial`, no users/groups/revocations,
/// timestamps formatted from the supplied epochs. `sig` empty (the caller signs with the org root).
pub fn empty_roster(org_id: &str, serial: u64, issued_epoch: i64, expires_epoch: i64) -> Roster {
    Roster {
        format: ROSTER_FORMAT.to_string(),
        org_id: org_id.to_string(),
        serial,
        issued_at: rfc3339(issued_epoch),
        expires_at: rfc3339(expires_epoch),
        groups: Vec::new(),
        users: Vec::new(),
        revoked_endpoints: Vec::new(),
        successor_root_pk: None,
        successor_sig: None,
        sig: String::new(),
    }
}

/// Format epoch seconds as an RFC3339 UTC instant (the schema's timestamp form) — the inverse of the
/// `validate` module's `parse_rfc3339`. chrono is a trust dep.
fn rfc3339(epoch: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Upsert a member + device (`org approve` / `devices add`; also user-key rotation), keyed by
/// `user_id`:
///  - NEW user_id → add the user with `[device]`.
///  - EXISTING user_id, SAME `user_pk` → APPEND `device` (dedup by endpoint), UNION the groups (a
///    `devices add` for an already-enrolled person).
///  - EXISTING user_id, DIFFERENT `user_pk` → REPLACE the user (fresh key + `[device]` + `groups`) —
///    the user-key rotation re-enrollment (same STABLE user_id, new key).
///
/// A COMPLETE pre-image of `validate_for_install` (rules 4a + 5a) AND its INTENDED semantic
/// (installable AND functional): rejects — BEFORE any mutation, with a clear typed
/// [`MutateError`] — a `user_id`
/// that is a group name, a requested group that is an existing user_id, a device endpoint already
/// under another user, AND a device endpoint that is currently REVOKED (re-adding it would produce a
/// roster `validate_for_install` ACCEPTS — rule 4b, revocation-wins — but where the device resolves to
/// nothing: a silently DEAD device; there is no un-revoke path by design, so the device must re-join with
/// a FRESH device key). So a roster the operator SIGNS can never be rejected by — or silently defeated
/// under — their own `RosterInstall`. DECLARES any new group at the top level (rule 5b). Clears `sig`.
/// `user_pk` / `device_endpoint_id` are the `b64u:` strings straight from the join code.
///
/// Trusts the caller to have already decoded/validated `device_endpoint_id` + `user_pk` as 32-byte
/// `b64u:` (the `org approve` caller runs `decode_endpoint_id` + `verify_device_binding` — which take
/// `&[u8; 32]` — before calling); a malformed endpoint would otherwise only surface as an `Encoding`
/// rejection at install. The revoked-endpoint guard compares the canonical `b64u:` strings (the same
/// form `revoke_device` stores), consistent with the rule-4a/dedup endpoint comparisons here.
pub fn upsert_member(
    roster: &mut Roster,
    user_id: &str,
    display_name: &str,
    user_pk: &str,
    groups: &[String],
    device_endpoint_id: &str,
    device_label: &str,
) -> Result<(), MutateError> {
    // ── Validation FIRST (a COMPLETE pre-image of `validate_for_install` rules 4a + 5a) so a
    //    mutation the operator SIGNS can never be rejected by their own `RosterInstall` with a
    //    cryptic post-sign error. All checks run against the CURRENT roster, before any mutation. ──
    // Rule 5a (this direction): the user_id must not equal any group name (existing top-level, or a
    // newly requested group).
    if roster.groups.iter().any(|g| g == user_id) || groups.iter().any(|g| g == user_id) {
        return Err(MutateError::UserIdIsGroup(user_id.to_string()));
    }
    // Rule 5a (the OTHER direction): a requested group must not equal an EXISTING user_id — else
    // `--groups <someone-else's-user-id>` would sign a roster `validate_for_install` rejects with
    // NamespaceCollision.
    for g in groups {
        if roster
            .users
            .iter()
            .any(|u| u.user_id != user_id && &u.user_id == g)
        {
            return Err(MutateError::GroupIsUserId(g.clone()));
        }
    }
    // Rule 4a: the device endpoint must not already belong to a DIFFERENT user — else a re-used /
    // spoofed join-code endpoint would sign a roster `validate_for_install` rejects with
    // DuplicateEndpoint. (An endpoint already under THIS user is the same-device dedup path below,
    // not a collision.)
    if roster.users.iter().any(|u| {
        u.user_id != user_id
            && u.devices
                .iter()
                .any(|d| d.endpoint_id == device_endpoint_id)
    }) {
        return Err(MutateError::EndpointBelongsToAnotherUser(
            device_endpoint_id.to_string(),
        ));
    }
    // Intended-semantic guard (beyond the acceptance rules): the endpoint must not be currently
    // REVOKED. Re-adding it — e.g. the same-user dedup/append path after a `revoke_device` removed it
    // from the user's device list but left it (append-only) in `revoked_endpoints` — yields a roster
    // `validate_for_install` ACCEPTS (rule 4b, revocation-wins) but where the endpoint resolves to
    // NOTHING: a silently dead device. Do NOT un-revoke (that defeats revocation; M3b has no un-revoke
    // path by design) — the device must re-join with a FRESH device key.
    if roster
        .revoked_endpoints
        .iter()
        .any(|e| e == device_endpoint_id)
    {
        return Err(MutateError::EndpointRevoked(device_endpoint_id.to_string()));
    }
    // ── Mutation (only after every invariant check passed). ──
    // Rule 5b: declare every requested group at the top level.
    for g in groups {
        if !roster.groups.iter().any(|x| x == g) {
            roster.groups.push(g.clone());
        }
    }
    let device = RosterDevice {
        endpoint_id: device_endpoint_id.to_string(),
        label: device_label.to_string(),
        role: "primary".to_string(),
    };
    if let Some(u) = roster.users.iter_mut().find(|u| u.user_id == user_id) {
        if u.user_pk == user_pk {
            // Same person, another device: append (dedup) + union groups.
            if !u
                .devices
                .iter()
                .any(|d| d.endpoint_id == device.endpoint_id)
            {
                u.devices.push(device);
            }
            for g in groups {
                if !u.groups.iter().any(|x| x == g) {
                    u.groups.push(g.clone());
                }
            }
        } else {
            // §4.6 rotation: replace the user with the fresh key + device + groups.
            u.display_name = display_name.to_string();
            u.user_pk = user_pk.to_string();
            u.groups = groups.to_vec();
            u.devices = vec![device];
        }
    } else {
        roster.users.push(RosterUser {
            user_id: user_id.to_string(),
            display_name: display_name.to_string(),
            user_pk: user_pk.to_string(),
            groups: groups.to_vec(),
            devices: vec![device],
        });
    }
    roster.sig.clear();
    Ok(())
}

/// Revoke ONE device (`org revoke alice/laptop`): remove the endpoint from its user's
/// device list AND add it to `revoked_endpoints` (revocation wins across all ALPNs — severing cuts even a
/// stale pair entry). The user entry (and their other devices) survive. Err on an unknown person/device.
pub fn revoke_device(
    roster: &mut Roster,
    user_id: &str,
    device_label: &str,
) -> Result<(), MutateError> {
    let user = roster
        .users
        .iter_mut()
        .find(|u| u.user_id == user_id)
        .ok_or_else(|| MutateError::NoSuchPerson(user_id.to_string()))?;
    let idx = user
        .devices
        .iter()
        .position(|d| d.label == device_label)
        .ok_or_else(|| MutateError::NoSuchDevice {
            user_id: user_id.to_string(),
            device_label: device_label.to_string(),
        })?;
    let dev = user.devices.remove(idx);
    if !roster
        .revoked_endpoints
        .iter()
        .any(|e| e == &dev.endpoint_id)
    {
        roster.revoked_endpoints.push(dev.endpoint_id);
    }
    roster.sig.clear();
    Ok(())
}

/// Remove a PERSON entirely (a departure, or a user-key rotation): drop the user
/// entry. `revoke_endpoints` = true (departing — hard cut) also pushes every device endpoint to
/// `revoked_endpoints`; false (rotation, `--user-key`) leaves them un-revoked so the SAME device can
/// re-enroll with a fresh user key (revoking them would trip validation rule 4b at re-approval). In
/// BOTH cases live sessions are severed on install (the removed endpoints are absent from the new
/// active-device set — the roster-resolved-but-absent sever arm). Err on an unknown person.
pub fn remove_user(
    roster: &mut Roster,
    user_id: &str,
    revoke_endpoints: bool,
) -> Result<(), MutateError> {
    let idx = roster
        .users
        .iter()
        .position(|u| u.user_id == user_id)
        .ok_or_else(|| MutateError::NoSuchPerson(user_id.to_string()))?;
    let user = roster.users.remove(idx);
    if revoke_endpoints {
        for d in user.devices {
            if !roster.revoked_endpoints.iter().any(|e| e == &d.endpoint_id) {
                roster.revoked_endpoints.push(d.endpoint_id);
            }
        }
    }
    roster.sig.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::sign::{mint_signed, verify};
    use crate::roster::validate::validate_for_install;
    use crate::roster::{decode_endpoint_id, encode_b64u};
    use ed25519_dalek::SigningKey;

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }
    const NOW: i64 = 1_760_000_000;
    fn ep(n: u8) -> String {
        encode_b64u(&[n; 32])
    }
    fn upk(n: u8) -> String {
        encode_b64u(&[n; 32])
    }

    #[test]
    fn empty_roster_is_serial_1_and_installs() {
        let r = empty_roster("acme", 1, NOW - 10, NOW + 90 * 86_400);
        assert_eq!(r.serial, 1);
        assert!(r.users.is_empty() && r.revoked_endpoints.is_empty() && r.groups.is_empty());
        assert_eq!(r.format, "mcpmesh-roster/1");
        // Signed, it passes full install validation (a valid empty roster).
        let signed = mint_signed(&root(), r);
        assert!(validate_for_install(&signed, &root().verifying_key(), 0, NOW).is_ok());
    }

    #[test]
    fn upsert_adds_a_new_member_declares_the_group_and_installs() {
        let mut r = empty_roster("acme", 1, NOW - 10, NOW + 86_400);
        r.serial = 2;
        upsert_member(
            &mut r,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        // The member is present, the group is declared top-level (rule 5b), sig is cleared.
        assert_eq!(r.users[0].user_id, "alice");
        assert_eq!(r.users[0].devices[0].endpoint_id, ep(2));
        assert!(r.groups.contains(&"team-eng".to_string()));
        assert!(r.sig.is_empty());
        let signed = mint_signed(&root(), r);
        let view = validate_for_install(&signed, &root().verifying_key(), 1, NOW).unwrap();
        assert_eq!(
            view.resolve(&decode_endpoint_id(&ep(2)).unwrap())
                .unwrap()
                .user_id,
            "alice"
        );
        verify(&signed, &root().verifying_key()).unwrap();
    }

    #[test]
    fn upsert_same_userpk_appends_a_device_and_unions_groups() {
        let mut r = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut r,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        // A `devices add` join code for the SAME person (same user_pk) appends the new device.
        upsert_member(
            &mut r,
            "alice",
            "Alice",
            &upk(1),
            &["team-research".into()],
            &ep(3),
            "desktop",
        )
        .unwrap();
        assert_eq!(r.users.len(), 1);
        assert_eq!(r.users[0].devices.len(), 2);
        assert!(r.users[0].groups.contains(&"team-research".to_string()));
        // Proven installable: BOTH appended devices resolve to alice.
        let signed = mint_signed(&root(), r);
        let view = validate_for_install(&signed, &root().verifying_key(), 1, NOW).unwrap();
        assert_eq!(
            view.resolve(&decode_endpoint_id(&ep(2)).unwrap())
                .unwrap()
                .user_id,
            "alice"
        );
        assert_eq!(
            view.resolve(&decode_endpoint_id(&ep(3)).unwrap())
                .unwrap()
                .user_id,
            "alice"
        );
    }

    #[test]
    fn upsert_different_userpk_rotates_the_user_key_keeping_the_user_id() {
        let mut r = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut r,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        // §4.6: re-enroll same user_id with a FRESH user key → the user is REPLACED (new pk, devices reset).
        upsert_member(
            &mut r,
            "alice",
            "Alice",
            &upk(9),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        assert_eq!(r.users.len(), 1);
        assert_eq!(r.users[0].user_pk, upk(9));
        assert_eq!(r.users[0].devices.len(), 1);
        // Proven installable after the key rotation: the endpoint resolves under the same user_id.
        let signed = mint_signed(&root(), r);
        let view = validate_for_install(&signed, &root().verifying_key(), 1, NOW).unwrap();
        assert_eq!(
            view.resolve(&decode_endpoint_id(&ep(2)).unwrap())
                .unwrap()
                .user_id,
            "alice"
        );
    }

    #[test]
    fn upsert_rejects_a_user_id_that_is_a_group_name() {
        let mut r = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        r.groups.push("team-eng".into());
        // "team-eng" as a user_id collides with the group (rule 5a) → Err before signing.
        assert!(upsert_member(&mut r, "team-eng", "X", &upk(1), &[], &ep(2), "laptop").is_err());
    }

    #[test]
    fn upsert_rejects_a_requested_group_that_is_an_existing_user_id() {
        // A COMPLETE pre-image of validate_for_install rule 5a (the OTHER direction): `--groups bob`
        // where "bob" is already a user_id would sign a roster the operator's own install rejects.
        let mut r = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut r,
            "bob",
            "Bob",
            &upk(3),
            &["team-eng".into()],
            &ep(4),
            "laptop",
        )
        .unwrap();
        assert!(
            upsert_member(
                &mut r,
                "alice",
                "Alice",
                &upk(1),
                &["bob".into()],
                &ep(2),
                "laptop"
            )
            .is_err(),
            "a requested group equal to an existing user_id must be rejected pre-sign"
        );
    }

    #[test]
    fn upsert_rejects_a_device_endpoint_already_under_another_user() {
        // A COMPLETE pre-image of validate_for_install rule 4a: a join-code endpoint already under
        // ANOTHER user would sign a roster the operator's own install rejects (DuplicateEndpoint).
        let mut r = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut r,
            "bob",
            "Bob",
            &upk(3),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        assert!(
            upsert_member(
                &mut r,
                "alice",
                "Alice",
                &upk(1),
                &["team-eng".into()],
                &ep(2),
                "desktop"
            )
            .is_err(),
            "a device endpoint already under another user must be rejected pre-sign"
        );
        // Control: the SAME user re-supplying their OWN endpoint is the dedup path, not a collision.
        assert!(
            upsert_member(
                &mut r,
                "bob",
                "Bob",
                &upk(3),
                &["team-eng".into()],
                &ep(2),
                "laptop"
            )
            .is_ok()
        );
    }

    #[test]
    fn revoke_device_moves_the_endpoint_to_revoked() {
        let mut r = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut r,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        r.serial = 3;
        revoke_device(&mut r, "alice", "laptop").unwrap();
        assert!(r.users[0].devices.is_empty());
        assert!(r.revoked_endpoints.contains(&ep(2)));
        // A revoked-but-installable roster: the endpoint resolves to NOTHING but IS revoked.
        let signed = mint_signed(&root(), r);
        let view = validate_for_install(&signed, &root().verifying_key(), 2, NOW).unwrap();
        let eid = decode_endpoint_id(&ep(2)).unwrap();
        assert!(view.resolve(&eid).is_none());
        assert!(view.is_revoked(&eid));
        // An unknown person/device is a typed Err, never a panic.
        assert!(revoke_device(&mut empty_roster("acme", 1, NOW, NOW), "nobody", "x").is_err());
    }

    #[test]
    fn remove_user_departing_revokes_all_devices_but_rotation_does_not() {
        // Departing (§4.5): drop the user AND revoke every device endpoint (hard cut).
        let mut departing = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut departing,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        remove_user(&mut departing, "alice", true).unwrap();
        assert!(departing.users.is_empty());
        assert!(departing.revoked_endpoints.contains(&ep(2)));
        // Proven installable: the departed endpoint resolves to NOTHING but IS revoked (hard cut).
        let signed = mint_signed(&root(), departing);
        let view = validate_for_install(&signed, &root().verifying_key(), 1, NOW).unwrap();
        let eid = decode_endpoint_id(&ep(2)).unwrap();
        assert!(view.resolve(&eid).is_none());
        assert!(view.is_revoked(&eid));

        // Rotation (§4.6, --user-key): drop the user but DON'T revoke the endpoint (same device re-enrolls).
        let mut rotating = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut rotating,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        remove_user(&mut rotating, "alice", false).unwrap();
        assert!(rotating.users.is_empty());
        assert!(!rotating.revoked_endpoints.contains(&ep(2)));
        assert!(remove_user(&mut rotating, "ghost", true).is_err());
    }

    #[test]
    fn upsert_rejects_a_currently_revoked_endpoint_but_rotation_reuse_is_ok() {
        // [Important] A currently-REVOKED endpoint must not be re-added. upsert → revoke_device →
        // upsert(same ep) would take the same-user dedup/append path (alice's devices no longer holds
        // ep2 after the revoke) and re-list ep2 under alice while ep2 is still in revoked_endpoints.
        // validate_for_install ACCEPTS that (rule 4b, revocation-wins) → the operator signs+installs a
        // roster where ep2 resolves to NOTHING: a silently DEAD device. Reject pre-sign.
        let mut r = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut r,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        revoke_device(&mut r, "alice", "laptop").unwrap();
        assert!(r.revoked_endpoints.contains(&ep(2)));
        assert!(
            upsert_member(
                &mut r,
                "alice",
                "Alice",
                &upk(1),
                &["team-eng".into()],
                &ep(2),
                "laptop"
            )
            .is_err(),
            "re-adding a currently-revoked endpoint must be rejected pre-sign (no silently-dead device)"
        );

        // Regression: the §4.6 rotation path (remove_user(revoke=false) → re-upsert the SAME endpoint)
        // never revokes the endpoint, so it must NOT trip the new guard — the same device re-enrolls
        // cleanly under a fresh user key, and the result installs.
        let mut rot = empty_roster("acme", 2, NOW - 10, NOW + 86_400);
        upsert_member(
            &mut rot,
            "alice",
            "Alice",
            &upk(1),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .unwrap();
        remove_user(&mut rot, "alice", false).unwrap();
        assert!(!rot.revoked_endpoints.contains(&ep(2)));
        upsert_member(
            &mut rot,
            "alice",
            "Alice",
            &upk(9),
            &["team-eng".into()],
            &ep(2),
            "laptop",
        )
        .expect("rotation re-enroll of an UN-revoked endpoint must succeed (no false positive)");
        let signed = mint_signed(&root(), rot);
        let view = validate_for_install(&signed, &root().verifying_key(), 1, NOW).unwrap();
        assert_eq!(
            view.resolve(&decode_endpoint_id(&ep(2)).unwrap())
                .unwrap()
                .user_id,
            "alice"
        );
    }
}
