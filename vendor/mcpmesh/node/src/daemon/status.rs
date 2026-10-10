//! The `status`-facing projections: live roster-mode status, the advisory presence read, and
//! the surface-clean service/peer views — all computed LIVE from the config, store,
//! and gate on each call, never from a cached snapshot.

use std::sync::Arc;

use mcpmesh_local_api::{BackendKind, PeerInfo, PresencePeer, RosterStatus, ServiceInfo};
use mcpmesh_trust::roster::validate::RosterState;

use crate::allowlist::PeerStore;
use crate::config::Config;
use crate::pairing;
use crate::util::epoch_now_i64;

use super::{MeshState, dial};

/// Live roster-mode status for `status`. **Computed LIVE from `mesh.roster.view()` on
/// each call — NOT a cached snapshot (DECLARED):** the roster view is already hot-swapped into the
/// gate on install, so a live read is cheap AND always-current, avoiding the display-only staleness
/// the pairing-grant snapshot path carries. Surface-clean: only org_id, serial, a plain state
/// word, and the org-root FINGERPRINT in short words — never raw keys/EndpointIds/roster path.
///
/// Three cases (DECLARED): (1) a roster is installed → the live `state` word. (2) NO roster installed
/// but an org root is PINNED (post-`join`, pre-approval) → `"pending"` with serial 0 + the
/// pinned org-root fingerprint, so `status` shows the anchor immediately after `join`. (3) a
/// pure-pairing daemon (no `org_root_pk` pin at all) → `None`, no roster block.
///
/// State mapping (DECLARED): `Approved → "approved"`, `DegradedGrace → "degraded"`,
/// `DegradedStopped → "stopped"`; no roster + pinned org → `"pending"`. The word is the gate's OWN
/// [`RosterGate::effective_state`] (expiry ∨ staleness) — the SAME computation `resolve`
/// decides on — so `status` reflects STALENESS, not just expiry.
///
/// Missing/unparseable pin (DECLARED): the org-root FINGERPRINT is derived from the pinned config
/// `org_root_pk`. If that pin is missing or unparseable (or the config read fails), the fingerprint
/// degrades GRACEFULLY to an empty string — NEVER a panic — and roster status still reports
/// org/serial/state (the render then omits the `org root:` line).
pub(crate) fn roster_status(mesh: &Arc<MeshState>, cfg: Option<&Config>) -> Option<RosterStatus> {
    // `cfg` is the caller's ALREADY-LOADED config (control.rs `status_result` loads it once for the
    // live service list and passes it through — the host polls status, so the double read mattered).
    // `None` models a transient read error, which must not fail `status`: fall back to an empty
    // fingerprint (and the "pending"/None cases below). Only the pinned org-root pk / org_id are
    // read from it — the state word comes from the gate's own `effective_state`.
    // The pinned org-root FINGERPRINT in short words. Decode the config b64u
    // `org_root_pk` → 32 bytes → fingerprint_words; a missing/unparseable pin → empty (no panic).
    let org_root_fingerprint = cfg
        .and_then(|c| c.identity.org_root_pk.as_deref())
        .and_then(|s| crate::roster::parse_org_root_pk(s).ok())
        .map(|vk| pairing::sas::fingerprint_words(&vk.to_bytes()))
        .unwrap_or_default();
    match mesh.roster.view() {
        Some(view) => {
            // The state word from the gate's OWN `effective_state` (expiry ∨ staleness)
            // — the SAME computation `resolve` decides on, so `status` reflects staleness, not just
            // expiry. `view` is Some here, so `effective_state` is Some (the `unwrap_or` never fires).
            let state = match mesh
                .roster
                .effective_state(epoch_now_i64())
                .unwrap_or(RosterState::Approved)
            {
                RosterState::Approved => "approved",
                RosterState::DegradedGrace => "degraded",
                RosterState::DegradedStopped => "stopped",
            };
            Some(RosterStatus {
                org_id: view.org_id().to_string(),
                serial: view.serial(),
                state: state.to_string(),
                org_root_fingerprint,
                // #93: the declared group namespace, so an embedder can enumerate the groups an
                // `allow` entry may name without hand-parsing the daemon-owned roster.json.
                groups: view.groups().to_vec(),
            })
        }
        None => {
            // Post-`join`, pre-approval: a pinned org root but no roster yet → "pending". A
            // pure-pairing daemon (no `org_root_pk` pin) has nothing to surface → None → no block.
            let cfg = cfg?;
            cfg.identity.org_root_pk.as_deref()?;
            Some(RosterStatus {
                org_id: cfg.identity.org_id.clone().unwrap_or_default(),
                serial: 0,
                state: "pending".to_string(),
                org_root_fingerprint,
                // Pending = pinned root, no roster yet. There is no group namespace to report:
                // groups are DECLARED by the roster document, and this node has none.
                groups: Vec::new(),
            })
        }
    }
}

/// The advisory presence read for `status`. Enumerates every ACTIVE roster device and
/// joins it with the presence table: display fields (user_id, device_label, role) come from the
/// installed roster; `online` is whether the table holds a LIVE (non-expired) heartbeat for that
/// endpoint (`PresenceTable::active`). ADVISORY-ONLY — a display convenience; NOTHING here authorizes
/// a dial. A device with no heartbeat reports `online: false` yet remains a full dial candidate
/// (absence never removes one). Empty in a pure-pairing daemon / before any roster is installed (the
/// field then serializes away). **Surface-clean:** the endpoint_id is used ONLY to join the
/// roster and presence tables — the output carries FLAT vocabulary alone (user_id/device_label/role/
/// online), never an EndpointId/pubkey/hash. Stable display order: by user, primary before mirror,
/// then label.
pub(crate) fn presence_peers(mesh: &Arc<MeshState>) -> Vec<PresencePeer> {
    let Some(view) = mesh.roster.view() else {
        return Vec::new();
    };
    let now = epoch_now_i64();
    // Map each active device to its freshest beat, so both `online` and the beat's app
    // metadata (#39) come from ONE table read.
    let active: std::collections::HashMap<[u8; 32], crate::roster::presence::PresenceEntry> =
        mesh.presence_table.active(now).into_iter().collect();
    // This node's OWN metadata is best-effort self-reported (a node does not receive its own
    // gossip, so it has no self entry in the table).
    let self_eid = *mesh.endpoint.id().as_bytes();
    let self_meta = mesh.app_metadata();
    let mut peers: Vec<PresencePeer> = view
        .devices()
        .map(|(eid, d)| PresencePeer {
            user_id: d.user_id.clone(),
            // #93: the roster carried both of these all along and this projection dropped them,
            // leaving an embedder a presence list it could label only with an opaque `user_id`.
            // Display data, exactly like `device_label` beside them — never an authz input.
            display_name: d.display_name.clone(),
            groups: d.groups.clone(),
            device_label: d.label.clone(),
            role: d.role.clone(),
            online: active.contains_key(eid),
            meta: if *eid == self_eid {
                self_meta.clone()
            } else {
                active.get(eid).map(|e| e.meta.clone()).unwrap_or_default()
            },
        })
        .collect();
    peers.sort_by(|a, b| {
        a.user_id
            .cmp(&b.user_id)
            .then_with(|| dial::dial_role_rank(&a.role).cmp(&dial::dial_role_rank(&b.role)))
            .then_with(|| a.device_label.cmp(&b.device_label))
    });
    peers
}

/// The `status`-facing view of the configured services (name, allow, backend KIND only — no
/// command/path). Malformed entries are omitted (they are not served either).
/// Map one authz principal to its human display form (#38): a `b64u:`/`eid:` principal
/// resolves through the peer entries to a display nickname; a bare string (roster
/// group/user_id) shows verbatim; an unresolvable stable principal renders a neutral
/// placeholder — porcelain shows THESE, never raw ids (surface discipline).
fn display_principal(principal: &str, peers: &[crate::allowlist::PeerEntry]) -> String {
    if principal.starts_with("eid:") {
        return peers
            .iter()
            .find(|p| mcpmesh_net::EndpointId::from_bytes(p.endpoint_id).principal() == principal)
            .map(|p| p.nickname.clone())
            .unwrap_or_else(|| "unpaired-device".to_owned());
    }
    if principal.starts_with("b64u:") {
        return peers
            .iter()
            .find(|p| p.user_id.as_deref() == Some(principal))
            .map(|p| p.nickname.clone())
            .unwrap_or_else(|| "unpaired-peer".to_owned());
    }
    principal.to_owned()
}

pub(crate) fn service_infos(
    mesh: &MeshState,
    peers: &[crate::allowlist::PeerEntry],
) -> Vec<ServiceInfo> {
    let live = mesh.live_services();
    // #100: the LIVE registry decides which services exist and what each admits — it is what the
    // accept path authorizes from. Reading config instead reported a hand-added service that had
    // not been reloaded as though it were servable.
    //
    // `ServiceEntry` carries only `allow` and an opaque backend, so the `backend` KIND and the
    // `ephemeral` flag are looked up from config / the ephemeral map as metadata for a name the
    // registry has already admitted. Ephemeral is checked first: it wins for a duplicate name,
    // matching `build_services_with_ephemeral`.
    //
    // #212: the registry's `allow` is the SECOND gate. `peer_revoke` writes only the revocation
    // table and never touches an entry, so copying `allow` out unfiltered reported grants the
    // first gate refuses before any service authz runs — #100's property, one gate short. Filter
    // through the same predicate the grant path refuses on, so the two surfaces agree; the entry
    // itself is untouched (unrevoking restores it with no re-grant), and a `peer_revoke` principal
    // still appears in `status.revoked`, so nothing is hidden — it is reported on the surface that
    // means it. (A roster-revoked device is filtered too; its record is the roster's
    // `revoked_endpoints`.)
    let roster = mesh.roster.view();
    let mut out: Vec<ServiceInfo> = live
        .iter()
        .map(|(name, entry)| {
            let allow: Vec<String> = entry
                .allow
                .iter()
                .filter(|p| {
                    !super::handlers::principal_is_revoked(&mesh.store, roster.as_deref(), p)
                })
                .cloned()
                .collect();
            ServiceInfo {
                name: name.clone(),
                allow_display: allow.iter().map(|p| display_principal(p, peers)).collect(),
                allow,
                backend: match entry.kind {
                    mcpmesh_net::ServiceKind::Socket => BackendKind::Socket,
                    _ => BackendKind::Run,
                },
                ephemeral: entry.ephemeral,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The service names the daemon KNOWS, live or pending a reload — config plus ephemeral
/// registrations (#100).
///
/// Deliberately NOT the live-registry view `service_infos` uses. `mint_invite` asks "is this a
/// known service name", and an invite is redeemed later, after reloads — so an invite for a service
/// the operator has just added to `config.toml` must still mint.
pub(crate) fn known_service_names(
    cfg: &Config,
    ephemeral: &std::collections::HashMap<String, crate::daemon::EphemeralService>,
) -> Vec<String> {
    let mut out: Vec<String> = cfg
        .services
        .iter()
        .filter(|(_, svc)| svc.backend_result().is_ok())
        .map(|(name, _)| name.clone())
        .collect();
    for name in ephemeral.keys() {
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
    out.sort();
    out
}

/// The `status`-facing view of known peers (nickname + granted services — never the
/// EndpointId). Fails open on a corrupt store row (see [`PeerStore::list`]).
pub(crate) fn peer_infos(store: &PeerStore) -> Vec<PeerInfo> {
    store
        .list()
        .unwrap_or_default()
        .into_iter()
        .map(|e| PeerInfo {
            name: e.nickname,
            services: e.services,
            // The peer's proven self-sovereign user_id (from a verified pairing binding), or `None`
            // for a nickname-only / `internal peer add` peer. A surface-clean opaque id, not a key.
            user_id: e.user_id,
            // The peer's stable DEVICE principal (#41) — the eid: the backend injects and the
            // allow lists use, so an embedder can map this nickname to the authenticated
            // endpoint. Always present (the endpoint id is the peer's row key).
            principal: Some(mcpmesh_net::EndpointId::from_bytes(e.endpoint_id).principal()),
        })
        .collect()
}

/// The `roster_members` read (#93): the org's declared groups + every person the installed roster
/// carries, with their devices and each device's live presence.
///
/// **Why this is not `status.presence`.** That projection enumerates DEVICES and answers "who is
/// reachable right now" — a person whose devices are all offline appears nowhere in it. An embedder
/// drawing a member list needs the opposite question ("who is in this org"), and had no way to ask
/// it: the roster is daemon-owned, so the only route was hand-parsing `<root>/config/roster.json`.
/// This answers the membership question and carries `online` per device, so one read serves both.
///
/// Reads the SAME validated view the gate resolves against, so a revoked device is absent here
/// exactly as it is unauthorized there — a member list that showed a revoked device as merely
/// offline would be worse than none.
///
/// ADVISORY: every field is display or authoring input. Empty in a pure-pairing daemon, and before
/// the first roster is installed.
pub(crate) fn roster_members(mesh: &Arc<MeshState>) -> mcpmesh_local_api::RosterMembersResult {
    let Some(view) = mesh.roster.view() else {
        return mcpmesh_local_api::RosterMembersResult::default();
    };
    let now = epoch_now_i64();
    let active: std::collections::HashMap<[u8; 32], crate::roster::presence::PresenceEntry> =
        mesh.presence_table.active(now).into_iter().collect();

    // Seed from the roster's USERS, not from the device map. That map is keyed by endpoint because
    // that is what the gate resolves, so a person whose devices are all revoked does not appear in
    // it at all — and revoking someone's only device is an ordinary operation that leaves their
    // user entry in the signed roster. Deriving members from devices made "revoked their last
    // device" indistinguishable from "removed from the org", which is the opposite of what a member
    // list is for.
    let mut by_user: std::collections::BTreeMap<String, mcpmesh_local_api::RosterMember> = view
        .users()
        .iter()
        .map(|u| {
            (
                u.user_id.clone(),
                mcpmesh_local_api::RosterMember {
                    user_id: u.user_id.clone(),
                    display_name: u.display_name.clone(),
                    groups: u.groups.clone(),
                    devices: Vec::new(),
                },
            )
        })
        .collect();
    for (eid, d) in view.devices() {
        // `or_insert_with` rather than a bare lookup: every device's owner is in `users` by
        // construction (both are read from one document), but a lookup that could silently DROP a
        // device is not worth the assumption.
        let member =
            by_user
                .entry(d.user_id.clone())
                .or_insert_with(|| mcpmesh_local_api::RosterMember {
                    user_id: d.user_id.clone(),
                    display_name: d.display_name.clone(),
                    groups: d.groups.clone(),
                    devices: Vec::new(),
                });
        member.devices.push(mcpmesh_local_api::RosterMemberDevice {
            label: d.label.clone(),
            role: d.role.clone(),
            // The handle a per-device `allow` entry names — the same `eid:` vocabulary
            // `PeerInfo::principal` carries. Included here, unlike on `PresencePeer`, because this
            // surface exists to be acted on: granting or revoking ONE device of a person needs a
            // way to name it, and roster mode has no nicknames.
            principal: mcpmesh_net::EndpointId::from_bytes(*eid).principal(),
            online: active.contains_key(eid),
        });
    }
    // Stable display order, matching `presence_peers`: primary before mirror, then label. The
    // view's device map is unordered, so without this the list reshuffles between reads.
    for m in by_user.values_mut() {
        m.devices.sort_by(|a, b| {
            dial::dial_role_rank(&a.role)
                .cmp(&dial::dial_role_rank(&b.role))
                .then_with(|| a.label.cmp(&b.label))
        });
    }
    mcpmesh_local_api::RosterMembersResult {
        groups: view.groups().to_vec(),
        // BTreeMap → ordered by `user_id`, which is the stable key. Sorting by `display_name`
        // would reorder the list when someone is renamed.
        users: by_user.into_values().collect(),
    }
}

#[cfg(test)]
mod tests {
    use crate::allowlist::PeerEntry;
    use crate::daemon::testutil::hermetic_mesh;

    /// `status` reflects the LIVE config + store on every call. A pairing grant
    /// (grant_service_access → allow-append) and a rendezvous PeerEntry write land durably
    /// WITHOUT touching `DaemonState`; `status` must still show the just-granted allow + the
    /// just-paired peer (the Jetson-proof "status says `allowed: no one yet` right after
    /// pairing" confusion).
    ///
    /// Drives the REAL `grant_service_access` rather than hand-writing the config. It used to do
    /// the latter and claim it was "exactly what grant does" — it was not: the real verb writes
    /// config AND reloads the live registry. #100 made that gap visible, because `status` now
    /// answers from the registry, so a bare config append no longer shows up (correctly — that is
    /// the state the accept path would refuse).
    #[tokio::test(flavor = "multi_thread")]
    async fn status_reads_the_live_config_and_store() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());

        // Durable mutations exactly as grant + rendezvous perform them: append the grant to the
        // config, and write the peer's PeerEntry straight to the store.
        crate::daemon::grant_service_access(&mesh, "alice", "alice", &["kb".to_string()])
            .await
            .unwrap();
        mesh.store
            .add(PeerEntry {
                endpoint_id: [9u8; 32],
                nickname: "alice".into(),
                services: Vec::new(),
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();

        // Status must reflect the LIVE truth.
        let status = crate::control::status_result(&state).unwrap();
        let kb = status
            .services
            .iter()
            .find(|s| s.name == "kb")
            .expect("kb service in status");
        assert!(
            kb.allow.contains(&"alice".to_string()),
            "status must show the live grant, got allow={:?}",
            kb.allow
        );
        let alice = status
            .peers
            .iter()
            .find(|p| p.name == "alice")
            .expect("status must show the live peer");
        // #41: the peer carries its stable eid: device principal — the eid of its stored
        // endpoint id ([9u8; 32] here) — so an embedder can map this nickname to the
        // authenticated endpoint the backend injects.
        assert_eq!(
            alice.principal.as_deref(),
            Some(
                mcpmesh_net::EndpointId::from_bytes([9u8; 32])
                    .principal()
                    .as_str()
            ),
            "peer principal must be the eid: of its endpoint id"
        );
    }

    /// Status surfaces self-sovereign identity (the adopted device->user binding): the daemon's OWN
    /// `self_user_id` (from its self-binding) and each paired peer's PROVEN `user_id` (from its
    /// `PeerEntry`). A peer that presented no binding stays nickname-only (`user_id: None`).
    #[tokio::test]
    async fn status_surfaces_self_and_peer_user_ids() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = []\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        mesh.set_self_binding(Some(crate::pairing::rendezvous::SelfBinding {
            user_pk: "b64u:selfpk".into(),
            sig: "b64u:selfsig".into(),
        }));
        // One peer that proved a self-sovereign user_id at pairing, one legacy nickname-only peer.
        mesh.store
            .add(PeerEntry {
                endpoint_id: [1u8; 32],
                nickname: "alice".into(),
                services: Vec::new(),
                paired_at: Some("1".into()),
                user_id: Some("b64u:alicepk".into()),
                last_addr: None,
            })
            .unwrap();
        mesh.store
            .add(PeerEntry {
                endpoint_id: [2u8; 32],
                nickname: "legacy".into(),
                services: Vec::new(),
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();

        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let status = crate::control::status_result(&state).unwrap();

        assert_eq!(
            status.self_user_id.as_deref(),
            Some("b64u:selfpk"),
            "status must surface this daemon's own self-sovereign user_id"
        );
        let alice = status
            .peers
            .iter()
            .find(|p| p.name == "alice")
            .expect("alice in status");
        assert_eq!(
            alice.user_id.as_deref(),
            Some("b64u:alicepk"),
            "a paired peer's PROVEN user_id must be surfaced in status"
        );
        let legacy = status
            .peers
            .iter()
            .find(|p| p.name == "legacy")
            .expect("legacy in status");
        assert!(
            legacy.user_id.is_none(),
            "a nickname-only peer stays user_id: None"
        );
    }

    /// The recent-pairings ring is BOUNDED (cap 8, oldest dropped), snapshots NEWEST FIRST, and
    /// `status_result` surfaces it (display-only ceremony state; empty in a control-only
    /// daemon — covered by control.rs's snapshot tests, whose StatusResult omits the field).
    #[tokio::test]
    async fn recent_pairings_ring_is_bounded_newest_first_and_surfaced_by_status() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        for i in 0..10u64 {
            mesh.record_pairing(mcpmesh_local_api::RecentPairing {
                peer_nickname: format!("peer{i}"),
                sas_code: format!("code-{i}"),
                paired_at_epoch: i,
                self_enroll: false,
            });
        }
        let recent = mesh.recent_pairings();
        assert_eq!(recent.len(), 8, "the ring is capped at 8");
        assert_eq!(recent[0].peer_nickname, "peer9", "newest first");
        assert_eq!(
            recent[7].peer_nickname, "peer2",
            "the two oldest were dropped"
        );

        let state = crate::control::DaemonState::with_mesh("test", mesh);
        let status = crate::control::status_result(&state).unwrap();
        assert_eq!(status.recent_pairings.len(), 8);
        assert_eq!(status.recent_pairings[0].sas_code, "code-9");
        assert_eq!(status.recent_pairings[0].paired_at_epoch, 9);
    }

    /// #212: `services[].allow` must not report a grant the accept path refuses on REVOCATION
    /// grounds — #100's property, with the second gate accounted for.
    ///
    /// The fixture seeds BOTH sides of the boundary in one allow: a device that will be revoked, a
    /// device that stays live, and the revoked device's `b64u:` identity. Revocation is driven
    /// through the REAL `peer_revoke` verb, so the table is written exactly as the gate reads it;
    /// the assertion is on the real `status_result`, and on the live registry staying untouched —
    /// this is a projection, not a strip.
    ///
    /// Deleting the filter in `service_infos` fails the first `assert_eq!` after the revoke: the
    /// revoked `eid:` reappears in `allow`.
    #[tokio::test(flavor = "multi_thread")]
    async fn status_hides_an_allow_entry_whose_principal_is_revoked() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mallory_eid = mcpmesh_net::EndpointId::from_bytes([3u8; 32]).principal();
        let alice_eid = mcpmesh_net::EndpointId::from_bytes([9u8; 32]).principal();
        std::fs::write(
            &config_path,
            format!(
                "[services.kb]\nsocket = \"/run/kb.sock\"\n\
                 allow = [\"{mallory_eid}\", \"{alice_eid}\", \"b64u:mallory\"]\n"
            ),
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
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
        let kb = |st: &mcpmesh_local_api::StatusResult| {
            st.services
                .iter()
                .find(|s| s.name == "kb")
                .cloned()
                .expect("kb service in status")
        };

        // Before: every entry shows — the fixture discriminates.
        let before = kb(&crate::control::status_result(&state).unwrap());
        assert_eq!(
            before.allow,
            vec![
                mallory_eid.clone(),
                alice_eid.clone(),
                "b64u:mallory".to_string()
            ]
        );
        assert_eq!(before.allow_display, vec!["mallory", "alice", "mallory"]);

        // Revoke the DEVICE by nickname: only that `eid:` is refused. The person is not revoked —
        // another of their devices would still be admitted through the `b64u:` — so it stays.
        crate::daemon::peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "mallory".into(),
                reason: Some("stolen".into()),
            },
        )
        .await
        .unwrap();
        let after_device = kb(&crate::control::status_result(&state).unwrap());
        assert_eq!(
            after_device.allow,
            vec![alice_eid.clone(), "b64u:mallory".to_string()],
            "a revoked eid: must not be reported as granted"
        );
        assert_eq!(
            after_device.allow_display,
            vec!["alice", "mallory"],
            "allow_display stays index-aligned with the filtered allow"
        );

        // Revoke the PERSON: the identity row lands, every known device is refused, and no new
        // device can attest — so the `b64u:` entry is honoured by nothing and goes too.
        crate::daemon::peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "b64u:mallory".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
        let status = crate::control::status_result(&state).unwrap();
        let after_user = kb(&status);
        assert_eq!(after_user.allow, vec![alice_eid.clone()]);
        assert_eq!(after_user.allow_display, vec!["alice"]);

        // The fact is not lost, only moved to the surface that means it…
        let revoked: Vec<&str> = status
            .revoked
            .iter()
            .map(|r| r.principal.as_str())
            .collect();
        assert!(
            revoked.contains(&mallory_eid.as_str()) && revoked.contains(&"b64u:mallory"),
            "status.revoked must still name both, got {revoked:?}"
        );
        // …and the live registry is untouched: a projection, not a strip. Unrevoking must restore
        // the grant without anyone re-granting it.
        let live = mesh.live_services();
        let entry = live.get("kb").expect("kb is live");
        assert_eq!(
            entry.allow,
            vec![
                mallory_eid.clone(),
                alice_eid.clone(),
                "b64u:mallory".to_string()
            ],
            "the filter must not mutate the registry"
        );
        assert_eq!(
            crate::config::Config::load(&config_path).unwrap().services["kb"]
                .allow
                .len(),
            3,
            "…nor the config"
        );
    }

    /// #212 + #218: a `b64u:` entry hides exactly while the IDENTITY is revoked, and `status` and
    /// the gate agree at every step. `peer_unrevoke` by nickname lifts one device's ENDPOINT row
    /// and leaves the identity row — and since #218 the gate refuses that device on the identity
    /// row alone, so the grant must stay hidden (under #212's original clause it reappeared here,
    /// which was true of a gate that never consulted `is_user_revoked`). Only `peer_unrevoke` on
    /// the identity restores both: the grant shows, and EVERY device resolves again.
    ///
    /// Asserted through the real gate (`mesh.gate.resolve`) beside the status filter, so the two
    /// cannot be shown agreeing on the wrong answer. Restoring #212's "every device refused"
    /// clause in `principal_is_revoked` fails the `is_empty` after the per-device unrevoke.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_grant_stays_hidden_until_the_identity_itself_is_unrevoked() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = [\"b64u:mallory\"]\n",
        )
        .unwrap();
        let mesh = hermetic_mesh(config_path.clone()).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        for (bytes, nickname) in [([3u8; 32], "mallory-laptop"), ([4u8; 32], "mallory-phone")] {
            mesh.store
                .add(PeerEntry {
                    endpoint_id: bytes,
                    nickname: nickname.into(),
                    services: Vec::new(),
                    paired_at: None,
                    user_id: Some("b64u:mallory".into()),
                    last_addr: None,
                })
                .unwrap();
        }
        let kb_allow = || {
            crate::control::status_result(&state)
                .unwrap()
                .services
                .into_iter()
                .find(|s| s.name == "kb")
                .expect("kb service in status")
                .allow
        };
        assert_eq!(kb_allow(), vec!["b64u:mallory".to_string()]);

        crate::daemon::peer_revoke(
            &state,
            mcpmesh_local_api::PeerRevokeParams {
                peer: "b64u:mallory".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
        assert!(
            kb_allow().is_empty(),
            "both devices and the identity are refused — nothing is granted"
        );
        let admits = |b: u8| mesh.gate.resolve(&[b; 32].into()).is_some();
        assert!(!admits(3) && !admits(4), "the gate refuses both devices");

        // Lift ONE device. The identity row stays (`status.revoked` still lists it), so the phone
        // is refused on the identity alone — the grant is worth nothing yet and must stay hidden.
        crate::daemon::peer_unrevoke(
            &state,
            mcpmesh_local_api::PeerUnrevokeParams {
                peer: "mallory-phone".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            mesh.store.is_user_revoked("b64u:mallory"),
            "fixture: the identity row must survive a per-device unrevoke"
        );
        assert!(
            !mesh.store.is_revoked(&[4u8; 32]),
            "fixture: the phone's ENDPOINT revocation is lifted"
        );
        assert!(
            !admits(4),
            "the identity revocation alone refuses the phone (#218)"
        );
        assert!(
            kb_allow().is_empty(),
            "a grant no device can use must not show — status and the gate agree"
        );

        // Lift the IDENTITY: the grant is live for every device of the person, and shows.
        crate::daemon::peer_unrevoke(
            &state,
            mcpmesh_local_api::PeerUnrevokeParams {
                peer: "b64u:mallory".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            admits(3) && admits(4),
            "unrevoking the identity restores admission for every device"
        );
        assert_eq!(
            kb_allow(),
            vec!["b64u:mallory".to_string()],
            "a grant an admitted device gets must show"
        );
    }
}

#[cfg(test)]
mod roster_members_tests {
    use super::roster_members;
    use crate::daemon::testutil::hermetic_mesh;
    use mcpmesh_trust::roster::validate::load_installed;
    use mcpmesh_trust::roster::{Roster, RosterDevice, RosterUser, encode_b64u, sign::mint_signed};

    /// Build + install a two-person roster: alice has two devices (one revoked), bob has one and no
    /// live device at all. Bob is the case `status.presence` cannot express.
    fn install_sample(mesh: &std::sync::Arc<crate::daemon::MeshState>) {
        let root = mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let r = mint_signed(
            &root,
            Roster {
                format: "mcpmesh-roster/1".into(),
                org_id: "acme".into(),
                serial: 5,
                issued_at: "2000-01-01T00:00:00Z".into(),
                expires_at: "2999-01-01T00:00:00Z".into(),
                groups: vec!["ops".into(), "eng".into()],
                users: vec![
                    RosterUser {
                        user_id: "alice".into(),
                        display_name: "Alice Example".into(),
                        user_pk: encode_b64u(&[1u8; 32]),
                        groups: vec!["eng".into()],
                        devices: vec![
                            // Deliberately listed MIRROR-first and with a label that sorts before
                            // the primary's, so the projection's sort is observable: document
                            // order, label order and role order all disagree here. With one device
                            // each (the first version of this fixture) the sort was a no-op and
                            // deleting it entirely left the suite green.
                            RosterDevice {
                                endpoint_id: encode_b64u(&[0xA3; 32]),
                                label: "aaa-tablet".into(),
                                role: "mirror".into(),
                            },
                            RosterDevice {
                                endpoint_id: encode_b64u(&[0xA1; 32]),
                                label: "laptop".into(),
                                role: "primary".into(),
                            },
                            RosterDevice {
                                endpoint_id: encode_b64u(&[0xA2; 32]),
                                label: "old-phone".into(),
                                role: "mirror".into(),
                            },
                        ],
                    },
                    RosterUser {
                        user_id: "bob".into(),
                        display_name: "Bob Example".into(),
                        user_pk: encode_b64u(&[2u8; 32]),
                        groups: vec!["ops".into(), "eng".into()],
                        devices: vec![RosterDevice {
                            endpoint_id: encode_b64u(&[0xB1; 32]),
                            label: "desktop".into(),
                            role: "primary".into(),
                        }],
                    },
                    // A member whose ONLY device is revoked — the ordinary outcome of
                    // `org_revoke <user>/<device>` on someone's last machine. Their user entry
                    // stays in the signed roster, so a member list that omits them reports
                    // "removed from the org" for someone who was not.
                    //
                    // `user_id` sorts AFTER bob but `display_name` sorts FIRST, so this also pins
                    // that the list is ordered by the stable id rather than by the name a rename
                    // could reshuffle.
                    RosterUser {
                        user_id: "zoe".into(),
                        display_name: "Aaron Deviceless".into(),
                        user_pk: encode_b64u(&[3u8; 32]),
                        groups: vec!["ops".into()],
                        devices: vec![RosterDevice {
                            endpoint_id: encode_b64u(&[0xC1; 32]),
                            label: "retired".into(),
                            role: "primary".into(),
                        }],
                    },
                ],
                // alice's `old-phone` is revoked — it must not appear as a member device at all.
                // zoe's ONLY device is revoked, which must not remove zoe.
                revoked_endpoints: vec![encode_b64u(&[0xA2; 32]), encode_b64u(&[0xC1; 32])],
                successor_root_pk: None,
                successor_sig: None,
                sig: String::new(),
            },
        );
        mesh.roster
            .install(load_installed(&r, &root.verifying_key()).unwrap());
    }

    /// #93(a): the membership read an embedder could not perform.
    ///
    /// The roster holds `display_name`, `groups`, and per-user devices; none of it crossed the
    /// control seam, so the only route to a member list was hand-parsing the daemon-owned
    /// `roster.json`. This asserts each field that was missing, and — the load-bearing part — that
    /// **bob appears at all**: he has no live device, so `status.presence` cannot show him, which
    /// is precisely why a presence list is not a member list.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_membership_read_carries_names_groups_and_offline_people() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = hermetic_mesh(cfg).await;
        install_sample(&mesh);

        let got = roster_members(&mesh);

        assert_eq!(
            got.groups,
            vec!["ops".to_string(), "eng".to_string()],
            "the DECLARED group namespace must be reported in document order — it is the set an \
             `allow` entry may name, and a UI has nothing else to offer from"
        );
        assert_eq!(
            got.users
                .iter()
                .map(|u| u.user_id.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bob", "zoe"],
            "every person in the roster must appear, ordered by the stable USER_ID. Three things \
             ride on this list. bob has no live device, which status.presence cannot express. \
             zoe's ONLY device is revoked — the ordinary result of `org_revoke <user>/<device>` on \
             someone's last machine, which leaves their user entry in the signed roster; deriving \
             members from the device map dropped zoe entirely, reporting 'removed from the org' \
             for someone who was not. And zoe sorts LAST by user_id but FIRST by display_name, so \
             this pins the ordering key against a rename reshuffling the list"
        );
        let zoe = &got.users[2];
        assert_eq!(zoe.display_name, "Aaron Deviceless");
        assert!(
            zoe.devices.is_empty(),
            "…and they appear with NO devices, which is the honest report: the entry exists, the \
             hardware is revoked"
        );
        assert_eq!(
            zoe.groups,
            vec!["ops".to_string()],
            "group membership survives the revocation of every device"
        );

        let alice = &got.users[0];
        assert_eq!(
            alice.display_name, "Alice Example",
            "the human name must cross the seam"
        );
        assert_eq!(alice.groups, vec!["eng".to_string()]);
        assert_eq!(
            alice
                .devices
                .iter()
                .map(|d| d.label.as_str())
                .collect::<Vec<_>>(),
            vec!["laptop", "aaa-tablet"],
            "TWO properties. (1) The order is primary-before-mirror — NOT document order (the \
             tablet is listed first) and NOT label order (`aaa-tablet` sorts first \
             alphabetically), so deleting the sort or sorting by label alone both fail here. \
             (2) a REVOKED device must be absent, not merely offline — the member list must agree \
             with what the gate would authorize"
        );
        assert_eq!(
            alice.devices[0].principal,
            mcpmesh_net::EndpointId::from_bytes([0xA1; 32]).principal(),
            "each device carries the eid: handle a per-device allow entry names — roster mode has \
             no nicknames, so without it one device cannot be addressed"
        );
        assert!(
            !alice.devices[0].online,
            "nothing has sent a heartbeat in this hermetic mesh"
        );

        let bob = &got.users[1];
        assert_eq!(bob.display_name, "Bob Example");
        assert_eq!(
            bob.groups,
            vec!["ops".to_string(), "eng".to_string()],
            "multi-group membership must survive verbatim"
        );
        assert_eq!(bob.devices.len(), 1);
    }

    /// A pure-pairing daemon has no roster, and must answer an EMPTY membership rather than
    /// erroring — an embedder polls one control surface whichever mode it is in.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_daemon_with_no_roster_reports_empty_membership() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = hermetic_mesh(cfg).await;

        let got = roster_members(&mesh);
        assert!(got.users.is_empty() && got.groups.is_empty());
    }

    /// #93(a), the other half: the same two fields on the PRESENCE projection, which is the
    /// surface an embedder already polls.
    ///
    /// Separate from the membership read on purpose — `presence_peers` builds its rows
    /// independently, so a fix to one says nothing about the other. Dropping either `.clone()` in
    /// that projection leaves the membership test above green.
    #[tokio::test(flavor = "multi_thread")]
    async fn presence_rows_carry_the_display_name_and_groups_too() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = hermetic_mesh(cfg).await;
        install_sample(&mesh);

        let peers = super::presence_peers(&mesh);
        let alice = peers
            .iter()
            .find(|p| p.user_id == "alice")
            .expect("alice's active device is projected");
        assert_eq!(alice.display_name, "Alice Example");
        assert_eq!(alice.groups, vec!["eng".to_string()]);
        assert_eq!(
            alice.device_label, "laptop",
            "the pre-existing fields must survive"
        );
    }

    /// #93(a): `status.roster` must carry the declared groups, so a UI can populate a group picker
    /// from the same read that tells it a roster exists.
    #[tokio::test(flavor = "multi_thread")]
    async fn roster_status_carries_the_declared_groups() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = hermetic_mesh(cfg).await;
        install_sample(&mesh);

        let st = super::roster_status(&mesh, None).expect("a roster is installed");
        assert_eq!(st.groups, vec!["ops".to_string(), "eng".to_string()]);
        assert_eq!(st.serial, 5, "the pre-existing fields must survive");
    }
}
