//! Principal → live-connection resolution for the revoke paths (#54).
//!
//! `service_allow_revoke` and `peer_remove` strip a principal from the config `allow` and reload,
//! which stops NEW sessions. Cutting the sessions already in flight needs the set of ENDPOINTS
//! that principal names, which is what this module computes — the input to
//! [`ConnRegistry::sever_matching`](mcpmesh_net::registry::ConnRegistry::sever_matching).
//!
//! Both identity sources are consulted, because either can be the thing an `allow` entry named:
//! the pairing [`PeerStore`] and the installed roster view. A roster-mode `allow` commonly holds a
//! bare roster `user_id` or a GROUP name, and neither has a `PeerEntry` — resolving only the store
//! would silently sever nothing for exactly those entries while the verb reported success.
//!
//! This is distinct from roster INSTALL severing
//! ([`install_roster_view_and_sever`](super::install_roster_view_and_sever)), which cuts devices a
//! new roster revoked or dropped. Here the operator explicitly withdrew a principal from a
//! service's allow, so that principal's devices are the target regardless of how they authenticate.

use std::collections::HashSet;

use mcpmesh_net::EndpointId;
use mcpmesh_trust::roster::validate::RosterView;

use crate::allowlist::PeerStore;

/// Every live-connection endpoint the stable `principal` names.
///
/// Four ways a principal can name an endpoint, unioned:
/// 1. a stored device whose `eid:` rendering matches (the device principal);
/// 2. a stored device carrying that `user_id` (a person covers all their paired devices);
/// 3. a roster device whose `eid:` rendering matches;
/// 4. a roster device whose `user_id` OR whose GROUP list contains the principal — the roster
///    vocabulary an `allow` entry may hold.
///
/// Matching RENDERS each endpoint rather than PARSING the principal, so every form is handled
/// without a new parser and an unrecognized principal resolves to the empty set — which severs
/// nothing. Failing to the empty set is the safe direction: over-severing would cut connections
/// the operator never revoked. A display nickname therefore never resolves here, exactly as it
/// never admits (#38).
///
/// Note the boundary: this is the *liveness* half of a revoke. The authorization half is the
/// config `allow` strip plus the live-registry swap; a peer whose `PeerEntry` is deleted outright
/// is already denied at gate resolve.
pub(crate) fn endpoints_for_principal(
    store: &PeerStore,
    roster: Option<&RosterView>,
    principal: &str,
) -> anyhow::Result<HashSet<EndpointId>> {
    let mut out = HashSet::new();
    for entry in store.list()? {
        let eid = EndpointId::from_bytes(entry.endpoint_id);
        if eid.principal() == principal || entry.user_id.as_deref() == Some(principal) {
            out.insert(eid);
        }
    }
    if let Some(view) = roster {
        for (bytes, device) in view.devices() {
            let eid = EndpointId::from_bytes(*bytes);
            if eid.principal() == principal
                || device.user_id == principal
                || device.groups.iter().any(|g| g == principal)
            {
                out.insert(eid);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allowlist::PeerEntry;

    /// A store holding `(endpoint byte, user_id, nickname)` rows.
    fn store_with(rows: &[(u8, Option<&str>, &str)]) -> (PeerStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = PeerStore::open(&tmp.path().join("state.redb")).expect("open store");
        for (b, user_id, nickname) in rows {
            store
                .add(PeerEntry {
                    endpoint_id: [*b; 32],
                    nickname: (*nickname).to_string(),
                    services: vec![],
                    paired_at: None,
                    user_id: user_id.map(|u| u.to_string()),
                    last_addr: None,
                })
                .expect("add peer");
        }
        (store, tmp)
    }

    fn eid(b: u8) -> EndpointId {
        EndpointId::from_bytes([b; 32])
    }

    fn set(ids: &[u8]) -> HashSet<EndpointId> {
        ids.iter().map(|b| eid(*b)).collect()
    }

    /// The PAIRING resolution rule: an `eid:` principal is ONE device, a `user_id` principal is
    /// EVERY device of that person, and an unknown principal is nothing (never over-severs).
    #[test]
    fn endpoints_for_principal_resolves_eid_and_user_id_and_nothing_else() {
        let (store, _tmp) = store_with(&[
            (1, Some("b64u:ann"), "ann-laptop"),
            (2, Some("b64u:ann"), "ann-phone"),
            (3, None, "bob"),
        ]);

        assert_eq!(
            endpoints_for_principal(&store, None, &eid(1).principal()).unwrap(),
            set(&[1]),
            "an eid: principal names exactly its own device"
        );
        assert_eq!(
            endpoints_for_principal(&store, None, "b64u:ann").unwrap(),
            set(&[1, 2]),
            "a user_id principal names every device of that person"
        );
        assert_eq!(
            endpoints_for_principal(&store, None, &eid(3).principal()).unwrap(),
            set(&[3]),
            "a device with no user_id still resolves by its eid:"
        );
        assert!(
            endpoints_for_principal(&store, None, "b64u:nobody")
                .unwrap()
                .is_empty(),
            "an unknown principal must sever nothing"
        );
        assert!(
            endpoints_for_principal(&store, None, "bob")
                .unwrap()
                .is_empty(),
            "a display nickname must never resolve — it never admits either (#38)"
        );
    }

    /// The ROSTER half: a roster `user_id` and a GROUP name each resolve to their devices even
    /// with NO `PeerEntry` — the case a store-only lookup severed nothing for, while the verb
    /// still reported success.
    #[test]
    fn endpoints_for_principal_resolves_roster_user_ids_and_groups() {
        use mcpmesh_trust::roster::sign::mint_signed;
        use mcpmesh_trust::roster::validate::load_installed;
        use mcpmesh_trust::roster::{Roster, RosterDevice, RosterUser, encode_b64u};

        let root = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let user = |uid: &str, group: &str, dev: u8| RosterUser {
            user_id: uid.into(),
            display_name: uid.into(),
            user_pk: encode_b64u(&[1u8; 32]),
            groups: vec![group.into()],
            devices: vec![RosterDevice {
                endpoint_id: encode_b64u(&[dev; 32]),
                label: "device".into(),
                role: "primary".into(),
            }],
        };
        let signed = mint_signed(
            &root,
            Roster {
                format: "mcpmesh-roster/1".into(),
                org_id: "acme".into(),
                serial: 1,
                issued_at: "2000-01-01T00:00:00Z".into(),
                expires_at: "2999-01-01T00:00:00Z".into(),
                groups: vec!["team-eng".into(), "team-ops".into()],
                users: vec![user("alice", "team-eng", 10), user("carol", "team-ops", 11)],
                revoked_endpoints: vec![],
                successor_root_pk: None,
                successor_sig: None,
                sig: String::new(),
            },
        );
        let view = load_installed(&signed, &root.verifying_key()).expect("valid roster view");

        // No PeerEntry at all — a pure roster node.
        let (store, _tmp) = store_with(&[]);

        assert_eq!(
            endpoints_for_principal(&store, Some(&view), "alice").unwrap(),
            set(&[10]),
            "a roster user_id resolves to that user's devices"
        );
        assert_eq!(
            endpoints_for_principal(&store, Some(&view), "team-eng").unwrap(),
            set(&[10]),
            "a roster GROUP name resolves to every device in the group"
        );
        assert_eq!(
            endpoints_for_principal(&store, Some(&view), &eid(11).principal()).unwrap(),
            set(&[11]),
            "a roster device also resolves by its eid: principal"
        );
        assert!(
            endpoints_for_principal(&store, Some(&view), "team-nope")
                .unwrap()
                .is_empty(),
            "an unknown group severs nothing"
        );
    }
}
