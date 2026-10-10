//! Org AUTHORING as control verbs (#66): `org_create`, `org_approve`, `org_revoke`.
//!
//! Roster mode's operator side used to exist only as CLI porcelain. `Request` carried
//! `roster_install` and `org_join` — both CONSUMER verbs — so an embedded node could read a roster
//! someone else authored and nothing more. An embedder that wanted managed group membership had to
//! shell out to the `mcpmesh` binary, which means shipping and version-matching a second artifact
//! and reproducing the enrollment UX in a terminal beside its own UI. Concretely, it could not
//! build an "approve this person" button, which is the single most obvious control a group product
//! needs.
//!
//! These are the same three operations the porcelain performed, moved behind the control seam so
//! both front doors run ONE implementation. `cli/src/enrollcmd.rs` now calls them.
//!
//! # What moved, and what did not
//!
//! The org root key stays exactly where it was — `<config_dir>/org-root.key`, 0600, derived here
//! from `mesh.config_path` the same way [`installed_roster_path`] derives `roster.json`, so a
//! `--profile` root and an embedded node each keep their own. The daemon runs as the caller's own
//! uid and the control socket is same-uid gated, so reading it here crosses no boundary the
//! porcelain did not already cross. What changes is that the key is now read by the process that
//! already owns every other piece of durable trust state, instead of by a second process racing it.
//!
//! The HUMAN half of each ceremony does not move and cannot: verifying that a join code came from
//! the person you think it did is an out-of-band conversation. What the verb returns is the
//! `join_code_fingerprint` the two humans compare — see [`OrgApproveResult`].
use anyhow::{Context, Result};
use mcpmesh_local_api::{OrgApproveResult, OrgCreateResult, OrgRevokeResult};
use mcpmesh_trust::keys::OrgRootKey;
use mcpmesh_trust::roster::sign::{mint_signed, sign, verify_device_binding};
use mcpmesh_trust::roster::{Roster, decode_b64u, decode_endpoint_id, encode_b64u, mutate};

use crate::control::DaemonState;
use crate::daemon::MeshState;
use crate::pairing;
use crate::roster::enroll::{JoinCode, OrgInviteCode};
use crate::util::{blocking, epoch_now_i64};

use super::roster_install::installed_roster_path;

/// The roster validity an `org_create` mints when the caller names none: 90 days.
///
/// Operator-grade, and a sharp edge at small scale — see [`OrgCreateParams::expires_secs`].
///
/// [`OrgCreateParams::expires_secs`]: mcpmesh_local_api::OrgCreateParams::expires_secs
pub(crate) const DEFAULT_EXPIRES_SECS: i64 = 90 * 86_400;

/// This node's org-root key path, derived from `config_path` exactly as
/// [`installed_roster_path`] derives the roster — so the key co-locates with the roster it signs
/// and stays per-node under a `--profile` root or an embedded node's directory.
///
/// Derived rather than stored: a separately-held path could be unset or stale, and an authoring
/// verb that signed with the wrong key — or minted a second root beside an existing one — would
/// orphan every roster already issued.
pub(crate) fn org_root_key_path(mesh: &MeshState) -> std::path::PathBuf {
    mesh.config_path
        .parent()
        .map(|dir| dir.join("org-root.key"))
        .unwrap_or_else(|| std::path::PathBuf::from("org-root.key"))
}

/// Load this operator's org root key + the installed roster — the two artifacts `approve` and
/// `revoke` mutate, re-sign, and install.
///
/// Refuses when either is absent, rather than creating one: an authoring verb on a node that is
/// not an operator is a caller mistake, and minting a root here would silently fork the org.
fn load_operator_roster(mesh: &MeshState) -> Result<(OrgRootKey, Roster)> {
    let key_path = org_root_key_path(mesh);
    anyhow::ensure!(
        key_path.exists(),
        "this node is not an org operator (no org root key); run org_create first"
    );
    let (root, _) = OrgRootKey::load_or_generate(&key_path)
        .map_err(|e| anyhow::anyhow!("org root key error at {}: {e}", key_path.display()))?;
    let roster_path = installed_roster_path(mesh);
    let bytes = std::fs::read(&roster_path)
        .with_context(|| format!("no installed roster at {}", roster_path.display()))?;
    let roster: Roster = serde_json::from_slice(&bytes).context("parse installed roster")?;
    Ok((root, roster))
}

/// Install a roster this node just SIGNED, through the one convergence path every other channel
/// uses ([`install_roster`]) — so an authored roster is validated, persisted, hot-swapped, severed,
/// and AUDITED identically to one that arrived by gossip or URL poll.
///
/// Staged through a temp file because `install_from_file` takes a path; the guard removes it on
/// every return, including the `?` early exits.
///
/// [`install_roster`]: super::roster_install::install_roster
async fn install_authored(
    state: &DaemonState,
    roster: &Roster,
    org_root_pk: Option<String>,
) -> Result<mcpmesh_local_api::RosterInstallResult> {
    let bytes = serde_json::to_vec(roster).context("serialize the authored roster")?;
    // The guard must outlive the install call, so it is bound here rather than in a helper.
    let staged = super::roster_install::write_temp_roster(
        &super::roster_install::roster_staging_dir(state.mesh_required()?),
        &bytes,
    )?;
    let path = staged.path().to_string_lossy().into_owned();
    let out = super::roster_install::install_roster(state, path, org_root_pk).await;
    drop(staged);
    out
}

/// `org_create`: mint the org root, sign an EMPTY roster at serial 1, install it (which pins the
/// root), and return the copyable invite + the root's fingerprint.
///
/// **One-time per node.** A second call is refused rather than replacing the key. Replacing it
/// would orphan every roster this node has already signed and silently invalidate every member's
/// pinned anchor — an outcome no caller could have wanted, arriving with no error.
pub(crate) async fn org_create(
    state: &DaemonState,
    name: String,
    expires_secs: Option<i64>,
    roster_url: Option<String>,
) -> Result<OrgCreateResult> {
    let mesh = state.mesh_required()?;
    anyhow::ensure!(!name.trim().is_empty(), "org_create: the org name is empty");
    // The org_id is the string every `allow` entry may name alongside groups and user_ids; a `/`
    // would collide with the `<user>/<device>` revoke grammar.
    anyhow::ensure!(
        !name.contains('/'),
        "org_create: the org name must not contain '/'"
    );
    // EVERY cheap check runs BEFORE the key is minted. `load_or_generate` PERSISTS a new key, and
    // the one-time guard then refuses the retry — so a validation failure after that point leaves
    // the node holding a root it cannot use and an error message ("one-time per node") that
    // actively discourages the only fix. Ordering is the whole mitigation: nothing below this line
    // can fail on caller input.
    let expires_secs = expires_secs.unwrap_or(DEFAULT_EXPIRES_SECS);
    anyhow::ensure!(
        expires_secs > 0,
        "org_create: expires_secs must be positive"
    );

    let key_path = org_root_key_path(mesh);
    let (root, created) = blocking("org_create root key", {
        let key_path = key_path.clone();
        move || OrgRootKey::load_or_generate(&key_path)
    })
    .await?
    .map_err(|e| anyhow::anyhow!("org root key error at {}: {e}", key_path.display()))?;
    anyhow::ensure!(
        created,
        "this node already holds an org root key ({}); org_create is one-time per node",
        key_path.display()
    );

    let now = epoch_now_i64();
    let roster = mint_signed(
        root.signing_key(),
        mutate::empty_roster(&name, 1, now, now.saturating_add(expires_secs)),
    );
    let org_root_pk = encode_b64u(&root.public_bytes());
    let installed = install_authored(state, &roster, Some(org_root_pk.clone())).await?;
    // Pin the roster URL through the same single-writer path the `set_roster_url` verb uses, so an
    // operator's poll loop keeps the hosted document current.
    //
    // BEST-EFFORT, deliberately. The org now EXISTS — root minted, roster installed, anchor pinned
    // — and `org_create` is one-time per node, so returning `Err` here would report failure for
    // something that fully succeeded and then refuse the retry. The URL is a convenience the
    // operator can set afterwards with `set_roster_url`; the org is not.
    if let Some(url) = &roster_url
        && let Err(e) = super::roster_install::set_roster_url(state, url.clone()).await
    {
        tracing::warn!(
            %e,
            "org created, but pinning the roster URL failed — set it with set_roster_url"
        );
    }
    Ok(OrgCreateResult {
        org_id: installed.org_id,
        serial: installed.serial,
        org_invite: OrgInviteCode {
            org_id: name,
            org_root_pk,
            roster_url,
        }
        .encode(),
        org_root_fingerprint: pairing::sas::fingerprint_words(&root.public_bytes()),
    })
}

/// `org_approve`: verify a join code's device→user-key binding, upsert the member, bump the serial,
/// re-sign, install.
///
/// **The binding is verified BEFORE any mutation.** A forged or corrupt code is refused while the
/// roster is still untouched, so a bad approval cannot leave a half-written document behind.
///
/// What this does NOT verify is that the code came from the person the operator believes it did —
/// nothing in the code binds it to a human. That check is the out-of-band read-back of
/// [`OrgApproveResult::join_code_fingerprint`], which is why the fingerprint is returned rather
/// than merely computed.
pub(crate) async fn org_approve(
    state: &DaemonState,
    join_code: String,
    groups: Vec<String>,
    user_id: Option<String>,
) -> Result<OrgApproveResult> {
    let mesh = state.mesh_required()?;
    // #66: serialize the whole authoring read-modify-write — see `MeshState::org_author_lock`.
    // Taken BEFORE the decode so two approvals queue rather than both reading the same serial.
    let _authoring = mesh.org_author_lock.lock().await;
    // ONE decode+verify+fingerprint path, shared with `org_join_code`. The operator confirms the
    // fingerprint THAT verb returned; if this computed its own from a second implementation, the
    // two could drift and the code confirmed would not be the code approved.
    let (jc, _user_pk, _device_id, code_fp) = inspect_join_code(&join_code)?;

    let (root, mut roster) = load_operator_roster(mesh)?;
    let uid = user_id.unwrap_or_else(|| jc.requested_user_id.clone());
    anyhow::ensure!(!uid.trim().is_empty(), "org_approve: the user_id is empty");
    // A `/` in a user_id HIJACKS the revoke grammar, and the id defaults to a value the person
    // being approved chose. Approving `alice/laptop` as a user_id means a later
    // `org_revoke("alice/laptop")` parses as "alice's laptop device" — so an operator trying to
    // remove the hostile entry instead cuts the real Alice's laptop and leaves the hostile user in
    // the roster, with `mode: "device"` reported and a confirmation sentence that reads correct for
    // both intents.
    //
    // Refused HERE rather than made unambiguous in `org_revoke`, because the ambiguity is in the
    // namespace: `user_id` and `<user_id>/<label>` cannot both be free-form and stay parseable.
    // Device labels may still contain `/` — with no `/` in any user_id, `split_once` at the FIRST
    // `/` always yields the right person.
    anyhow::ensure!(
        !uid.contains('/'),
        "org_approve: a user_id must not contain '/' (it would collide with the \
         '<user_id>/<device>' revoke grammar); pass an explicit user_id to override the one the \
         join code requested"
    );

    roster.serial += 1;
    mutate::upsert_member(
        &mut roster,
        &uid,
        &jc.display_name,
        &jc.user_pk,
        &groups,
        &jc.device_endpoint_id,
        &jc.device_label,
    )
    .map_err(|e| anyhow::anyhow!("roster mutation rejected: {e}"))?;
    sign(root.signing_key(), &mut roster).map_err(|e| anyhow::anyhow!("sign roster: {e}"))?;

    let installed = install_authored(state, &roster, None).await?; // root already pinned
    Ok(OrgApproveResult {
        user_id: uid,
        groups,
        org_id: installed.org_id,
        serial: installed.serial,
        join_code_fingerprint: code_fp,
    })
}

/// The decoded, binding-VERIFIED parts of a join code, shared by [`org_join_code`] and
/// [`org_approve`] so the fingerprint an operator confirms and the one the approval acts on are
/// computed from the same bytes by the same code.
fn inspect_join_code(join_code: &str) -> Result<(JoinCode, [u8; 32], [u8; 32], String)> {
    // No added context: the decode error is already the user-facing sentence.
    let jc = JoinCode::decode(join_code)?;
    let user_pk = decode_endpoint_id(&jc.user_pk).context("join code has an invalid user_pk")?;
    let device_id = decode_endpoint_id(&jc.device_endpoint_id)
        .context("join code has an invalid device endpoint")?;
    let sig = decode_b64u(&jc.binding_sig).context("join code has an invalid signature")?;
    verify_device_binding(&user_pk, &device_id, &sig).map_err(|_| {
        anyhow::anyhow!("join code device binding failed — the code is forged or corrupt")
    })?;
    let fingerprint = pairing::sas::join_code_fingerprint(&user_pk, &device_id);
    Ok((jc, user_pk, device_id, fingerprint))
}

/// `org_join_code`: INSPECT a join code — what it claims, and the fingerprint that decides whether
/// to believe it. Read-only; nothing is signed, installed, or persisted.
///
/// This is what makes an embedder's "approve this person" button CORRECT rather than merely
/// possible. The fingerprint must be confirmed out-of-band BEFORE the approval — a substituted code
/// is caught there or not at all — and reading it off `org_approve`'s result is too late, since the
/// member is already in the signed roster by then. The CLI always could do this locally; an
/// embedder could not, because the join-code format lives here and not on the control seam.
///
/// The binding is still verified, so a forged code is REFUSED rather than described.
pub(crate) async fn org_join_code(
    state: &DaemonState,
    join_code: String,
) -> Result<mcpmesh_local_api::OrgJoinCodeResult> {
    // A mesh is required for consistency with the approval this precedes: an inspection that
    // worked on a control-only daemon would have different preconditions than the verb it exists
    // to guard.
    let _ = state.mesh_required()?;
    let (jc, _user_pk, _device_id, fingerprint) = inspect_join_code(&join_code)?;
    Ok(mcpmesh_local_api::OrgJoinCodeResult {
        // Sender-chosen claims, echoed for display and never trusted. What is verified is the
        // binding; what is checkable is the fingerprint.
        display_name: jc.display_name,
        requested_user_id: jc.requested_user_id,
        device_label: jc.device_label,
        join_code_fingerprint: fingerprint,
    })
}

/// `org_revoke`: mutate the installed roster per the target grammar, bump, re-sign, install — which
/// SEVERS the cut devices' live sessions (#54), not merely refuses their next one.
///
/// Three readings, and the difference between them is destructive, so `mode` is reported back:
/// `"<user>/<device>"` cuts one device; a bare `user_id` removes the person AND revokes every
/// device; `user_key = true` removes the person while leaving their devices un-revoked, which is
/// the rotation case — the same hardware re-enrolls under a fresh user key.
pub(crate) async fn org_revoke(
    state: &DaemonState,
    target: String,
    user_key: bool,
) -> Result<OrgRevokeResult> {
    let mesh = state.mesh_required()?;
    let _authoring = mesh.org_author_lock.lock().await;
    anyhow::ensure!(!target.trim().is_empty(), "org_revoke: the target is empty");
    // `user_key` is a rotation of the PERSON's key, so a device-shaped target is contradictory
    // rather than merely unmatched. Refused by name instead of falling through to a
    // `no such person 'alice/laptop'`, which reads like the person is missing.
    anyhow::ensure!(
        !(user_key && target.contains('/')),
        "org_revoke: user_key rotates a PERSON's key, so the target must be a user_id, not \
         '<user_id>/<device>'"
    );
    let (root, mut roster) = load_operator_roster(mesh)?;
    roster.serial += 1;
    let mode = if user_key {
        // Rotation: remove the person, keep the devices un-revoked so the same hardware can
        // re-enroll. Revoking them here is the difference between "rotate a key" and "lock this
        // person out of their own laptop".
        mutate::remove_user(&mut roster, &target, false).map_err(|e| anyhow::anyhow!("{e}"))?;
        "user-key-rotation"
    } else if let Some((person, device)) = target.split_once('/') {
        mutate::revoke_device(&mut roster, person, device).map_err(|e| anyhow::anyhow!("{e}"))?;
        "device"
    } else {
        // Departing: remove AND revoke every device (the hard cut).
        mutate::remove_user(&mut roster, &target, true).map_err(|e| anyhow::anyhow!("{e}"))?;
        "person"
    };
    sign(root.signing_key(), &mut roster).map_err(|e| anyhow::anyhow!("sign roster: {e}"))?;

    let installed = install_authored(state, &roster, None).await?;
    Ok(OrgRevokeResult {
        target,
        mode: mode.to_string(),
        org_id: installed.org_id,
        serial: installed.serial,
        severed: installed.severed,
    })
}

/// `org_rotate` (#93 ask c): mint a SUCCESSOR org root and publish the bridge to it.
///
/// The org's trust anchor is one pinned key. Until now nothing could move it: an operator laptop
/// that died took the org with it 90 days later, when the roster expired — and the delay is what
/// made it hard to diagnose. Recovery was O(N) fresh ceremonies with every member.
///
/// What this publishes is a roster **signed by the successor**, carrying `successor_root_pk` and a
/// `successor_sig` by the CURRENT root over `domain ∥ org_id ∥ successor_pk`. A member still pinned
/// to the current root verifies that cross-signature with the key it already has, adopts the
/// successor, and then verifies the body with it.
///
/// **The bridge rides every subsequent roster**, which is the property that makes rotation
/// survivable rather than merely possible: a member offline for one publication still catches up.
/// A member two rotations behind needs a fresh `org_join` — chaining further would mean carrying a
/// history.
///
/// The successor is staged at `<config>/org_root_next.key` and PROMOTED to `org-root.key` BEFORE
/// the roster is published, with a rollback if publishing then fails. The first cut promoted last,
/// "so a half-finished rotation is a no-op" — and the 0.47.0 gate proved the opposite: publishing
/// pins the new anchor and gossip-announces the roster, so a failed rename left this node pinned to
/// the successor while still signing with the predecessor, and the org could no longer publish
/// membership changes at all.
pub(crate) async fn org_rotate(
    state: &DaemonState,
    new_key_path: Option<String>,
) -> Result<mcpmesh_local_api::OrgRotateResult> {
    let mesh = state.mesh_required()?;
    let _authoring = mesh.org_author_lock.lock().await;
    let (current, mut roster) = load_operator_roster(mesh)?;

    let key_path = org_root_key_path(mesh);
    let next_path = match new_key_path {
        Some(p) => std::path::PathBuf::from(p),
        None => key_path.with_file_name("org_root_next.key"),
    };
    // Generated if absent, reused if the operator staged one — so a rotation can be prepared on a
    // machine that is not the one publishing.
    let (successor, _created) = OrgRootKey::load_or_generate(&next_path)
        .map_err(|e| anyhow::anyhow!("successor root key error at {}: {e}", next_path.display()))?;
    let successor_pk = successor.public_bytes();
    anyhow::ensure!(
        successor_pk != current.public_bytes(),
        "the successor key is the same as the current root — rotation would be a no-op, and \
         publishing it would tell every member to re-anchor to the key they already have"
    );

    // The bridge, signed by the CURRENT root. This is what a member still pinned to it verifies.
    let cross = mcpmesh_trust::roster::sign::sign_org_rotation(
        current.signing_key(),
        &roster.org_id,
        &successor_pk,
    );
    roster.serial += 1;
    roster.successor_root_pk = Some(mcpmesh_trust::roster::encode_b64u(&successor_pk));
    roster.successor_sig = Some(mcpmesh_trust::roster::encode_b64u(&cross));
    // A rotation declares the ROTATION FORMAT. The schema's own rule is that additive fields are a
    // format bump, and honouring it turns "unknown field" on a pre-0.47.0 member into "unexpected
    // roster format", which is the difference between an operator upgrading and an operator hunting
    // for corruption.
    roster.format = mcpmesh_trust::roster::ROSTER_FORMAT_ROTATION.to_string();
    // Signed by the SUCCESSOR: from here on the new key is the one that signs.
    sign(successor.signing_key(), &mut roster)
        .map_err(|e| anyhow::anyhow!("sign roster with the successor root: {e}"))?;

    // PROMOTE FIRST, then publish — and roll back if publishing fails.
    //
    // The first cut promoted last, "so a half-finished rotation is a no-op". The gate proved the
    // opposite: `install_authored` pins the new anchor AND gossip-announces the roster, so a failed
    // rename (a `--new-key` on another filesystem, a read-only config dir, any I/O error) left the
    // operator PINNED TO THE SUCCESSOR WHILE STILL HOLDING THE PREDECESSOR AS ITS SIGNING KEY. The
    // next `org_approve` would then sign with a key its own node rejects, and the org could no
    // longer publish membership changes at all.
    //
    // Promoting first inverts the failure: if the rename fails, nothing has been published and the
    // roster is untouched — a genuine no-op. If the publish then fails, we restore the predecessor,
    // and the worst case is a staged key left behind, which the next rotation reuses harmlessly.
    let (np, kp) = (next_path.clone(), key_path.clone());
    let predecessor_bytes = crate::util::blocking("join org root promote", move || {
        let prior = std::fs::read(&kp).ok();
        std::fs::rename(&np, &kp)
            .with_context(|| format!("promote {} to {}", np.display(), kp.display()))?;
        anyhow::Ok(prior)
    })
    .await??;

    let new_pk_b64u = mcpmesh_trust::roster::encode_b64u(&successor_pk);
    let installed = match install_authored(state, &roster, Some(new_pk_b64u.clone())).await {
        Ok(v) => v,
        Err(e) => {
            // Put the predecessor back, so the operator is exactly where they started.
            if let Some(prior) = predecessor_bytes {
                let kp = key_path.clone();
                let _ = crate::util::blocking("join org root rollback", move || {
                    let bytes: [u8; 32] = prior
                        .as_slice()
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("saved predecessor key is not 32 bytes"))?;
                    mcpmesh_trust::keys::write_signing_key(
                        &kp,
                        &mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&bytes),
                        true,
                    )
                    .map_err(|e| anyhow::anyhow!("restore org root key: {e}"))
                })
                .await;
            }
            return Err(e.context(
                "the rotation was NOT published; this node's org root key has been restored",
            ));
        }
    };

    tracing::warn!(
        org_id = %installed.org_id,
        serial = installed.serial,
        new_root = %new_pk_b64u,
        "ORG ROOT ROTATED — members re-anchor as they receive this roster"
    );
    Ok(mcpmesh_local_api::OrgRotateResult {
        org_id: installed.org_id,
        serial: installed.serial,
        new_root_pk: new_pk_b64u,
        old_root_fingerprint: crate::pairing::sas::fingerprint_words(&current.public_bytes()),
        new_root_fingerprint: crate::pairing::sas::fingerprint_words(&successor_pk),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::testutil::hermetic_mesh;
    use mcpmesh_trust::ed25519_dalek::SigningKey;

    async fn operator_state(dir: &std::path::Path) -> DaemonState {
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let mesh = hermetic_mesh(config_path).await;
        DaemonState::with_mesh("test", mesh)
    }

    fn join_code_signed_by(
        signer: &SigningKey,
        claimed_user_pk: &[u8; 32],
        device: &[u8; 32],
    ) -> String {
        let sig = mcpmesh_trust::roster::sign::sign_device_binding(signer, device);
        JoinCode {
            display_name: "Alice".into(),
            requested_user_id: "alice".into(),
            user_pk: encode_b64u(claimed_user_pk),
            device_endpoint_id: encode_b64u(device),
            device_label: "laptop".into(),
            binding_sig: encode_b64u(&sig),
        }
        .encode()
    }

    /// #66: an embedder can author an org end to end — create, approve, revoke — over the control
    /// seam alone, with no `mcpmesh` binary anywhere.
    ///
    /// This is the whole issue: the operator side existed only as porcelain, so an embedded node
    /// could consume a roster and never author one. The assertions follow the membership through
    /// `roster_members`, which is what an "approve this person" button would actually render.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedder_can_create_approve_and_revoke_over_the_control_seam() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;

        let created = org_create(&state, "acme".into(), Some(86_400), None)
            .await
            .expect("org_create mints the root and installs an empty roster");
        assert_eq!(created.org_id, "acme");
        assert_eq!(created.serial, 1, "a fresh org starts at serial 1");
        assert!(
            created.org_invite.starts_with("mcpmesh-org:"),
            "the copyable invite must be the artifact a joiner pastes: {}",
            created.org_invite
        );
        assert!(
            !created.org_root_fingerprint.is_empty(),
            "the fingerprint anchors every joiner's trust and must be shown to the operator"
        );

        // One-time per node: a second create must REFUSE, not replace the key. Replacing it would
        // orphan every roster already signed and silently invalidate every member's pinned anchor.
        let again = org_create(&state, "acme2".into(), None, None)
            .await
            .expect_err("a second org_create must be refused");
        assert!(
            format!("{again:#}").contains("one-time per node"),
            "it must be refused BY THE ONE-TIME GUARD, naming the existing key. Asserting only \
             `is_err()` here proved nothing: without the guard the second create still fails, on \
             the roster serial check, for a reason that has nothing to do with the orphaned root \
             — and the orphaning would already have happened. Got: {again:#}"
        );

        // The org must declare a group before anyone can be approved into it: rule 5b refuses a
        // roster whose user carries an undeclared group, which would make `allow = ["eng"]`
        // ambiguous. So approve with NO groups here, which is the shape a fresh org supports.
        let alice_key = SigningKey::from_bytes(&[9u8; 32]);
        let alice_pk = alice_key.verifying_key().to_bytes();
        let device = [42u8; 32];
        let code = join_code_signed_by(&alice_key, &alice_pk, &device);

        let approved = org_approve(&state, code, vec![], None)
            .await
            .expect("a well-formed join code is approved");
        assert_eq!(
            approved.user_id, "alice",
            "the requested user_id is accepted by default"
        );
        assert_eq!(approved.serial, 2, "an approval bumps the serial");
        assert!(
            !approved.join_code_fingerprint.is_empty(),
            "the fingerprint is the ONLY thing binding this code to a person, and only a human can \
             check it — so it must be returned, not merely computed"
        );

        // …and it is visible through the READ surface an embedder renders (#93).
        let mesh = state.mesh_required().unwrap();
        let members = crate::daemon::roster_members(mesh);
        let alice = members
            .users
            .iter()
            .find(|u| u.user_id == "alice")
            .expect("the approved member appears in the membership read");
        assert_eq!(
            alice.display_name, "Alice",
            "the join code's display name is carried through"
        );
        assert_eq!(alice.devices.len(), 1);
        assert_eq!(alice.devices[0].label, "laptop");

        // Revoke the person: removed AND every device revoked (the hard cut).
        let revoked = org_revoke(&state, "alice".into(), false)
            .await
            .expect("revoke installs");
        assert_eq!(
            revoked.mode, "person",
            "a bare user_id is the departing-person grammar"
        );
        assert_eq!(revoked.serial, 3);
        let after = crate::daemon::roster_members(mesh);
        assert!(
            after.users.is_empty(),
            "the revoked person must be gone from the membership read: {:?}",
            after.users
        );
    }

    /// #66: the user-key ROTATION grammar is not the departure grammar, and the difference is
    /// destructive.
    ///
    /// Both remove the person. Only the departure revokes their devices — a rotation must leave
    /// them un-revoked so the SAME hardware re-enrolls under a fresh user key. Getting this
    /// backwards locks someone out of their own laptop, or leaves a departed employee's device
    /// admissible; `mode` is reported back so a caller can confirm which reading it got.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_rotation_leaves_the_devices_usable_and_a_departure_does_not() {
        let alice_key = SigningKey::from_bytes(&[9u8; 32]);
        let alice_pk = alice_key.verifying_key().to_bytes();
        let device = [42u8; 32];

        // Rotation.
        let d1 = tempfile::tempdir().unwrap();
        let s1 = operator_state(d1.path()).await;
        org_create(&s1, "acme".into(), None, None).await.unwrap();
        org_approve(
            &s1,
            join_code_signed_by(&alice_key, &alice_pk, &device),
            vec![],
            None,
        )
        .await
        .unwrap();
        let rot = org_revoke(&s1, "alice".into(), true).await.unwrap();
        assert_eq!(rot.mode, "user-key-rotation");
        let m1 = s1.mesh_required().unwrap();
        assert!(
            !m1.roster.view().unwrap().is_revoked(&device),
            "a ROTATION must leave the device un-revoked — the same hardware re-enrolls under a \
             fresh user key, and revoking it here locks the person out of their own machine"
        );

        // Departure, same starting state.
        let d2 = tempfile::tempdir().unwrap();
        let s2 = operator_state(d2.path()).await;
        org_create(&s2, "acme".into(), None, None).await.unwrap();
        org_approve(
            &s2,
            join_code_signed_by(&alice_key, &alice_pk, &device),
            vec![],
            None,
        )
        .await
        .unwrap();
        let dep = org_revoke(&s2, "alice".into(), false).await.unwrap();
        assert_eq!(dep.mode, "person");
        let m2 = s2.mesh_required().unwrap();
        assert!(
            m2.roster.view().unwrap().is_revoked(&device),
            "a DEPARTURE must revoke the device — otherwise the person's hardware stays admissible \
             after they are removed"
        );
    }

    /// #66: a forged join code dies on the BINDING check, before any roster or operator state is
    /// touched.
    ///
    /// Relocated from the porcelain when the logic moved behind the seam — the property is the
    /// same, and it is about ORDER: this node has no org root key at all, so if the binding check
    /// ran after `load_operator_roster` the error would be "not an org operator". Getting the
    /// binding error instead proves a substituted code is refused while the roster is untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_forged_binding_is_refused_before_any_operator_state_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await; // NO org_create — not an operator

        // Mallory signs the binding with HER key while the code claims Alice's user_pk — exactly
        // the substitution the binding check exists to catch.
        let mallory = SigningKey::from_bytes(&[7u8; 32]);
        let alice_pk = SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes();
        let code = join_code_signed_by(&mallory, &alice_pk, &[42u8; 32]);

        let err = org_approve(&state, code, vec![], None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("device binding failed"),
            "the forged binding must be the failure — not 'not an org operator', which would mean \
             the check runs after the roster load: {msg}"
        );
    }

    /// #66: a `/` in a user_id HIJACKS the revoke grammar — proven, then closed.
    ///
    /// The id defaults to a value the person being APPROVED chose. Approving `alice/laptop` as a
    /// user_id means a later `org_revoke("alice/laptop")` parses as "alice's laptop device": the
    /// operator trying to remove the hostile entry instead cuts the real Alice's laptop and leaves
    /// the hostile user in the roster, with `mode: "device"` and a confirmation sentence that reads
    /// correct for both intents.
    ///
    /// Refused at approval, where the namespace is decided. Removing the guard makes the second
    /// approval below succeed, which is the whole exploit.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_id_containing_a_slash_cannot_hijack_the_revoke_grammar() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;
        org_create(&state, "acme".into(), None, None).await.unwrap();

        let alice = SigningKey::from_bytes(&[9u8; 32]);
        let alice_pk = alice.verifying_key().to_bytes();
        org_approve(
            &state,
            join_code_signed_by(&alice, &alice_pk, &[0xA1; 32]),
            vec![],
            None,
        )
        .await
        .expect("the real alice is approved");

        // Mallory asks to be called `alice/laptop`.
        let mallory = SigningKey::from_bytes(&[7u8; 32]);
        let mallory_pk = mallory.verifying_key().to_bytes();
        let hostile = JoinCode {
            display_name: "Mallory".into(),
            requested_user_id: "alice/laptop".into(),
            user_pk: encode_b64u(&mallory_pk),
            device_endpoint_id: encode_b64u(&[0xB1; 32]),
            device_label: "phone".into(),
            binding_sig: encode_b64u(&mcpmesh_trust::roster::sign::sign_device_binding(
                &mallory,
                &[0xB1; 32],
            )),
        }
        .encode();

        let err = org_approve(&state, hostile.clone(), vec![], None)
            .await
            .expect_err("a user_id carrying '/' must be refused");
        assert!(
            format!("{err:#}").contains("must not contain '/'"),
            "refused for the RIGHT reason — the grammar collision, not some incidental failure: \
             {err:#}"
        );

        // …and the operator's override is the documented way through, so the person can still be
        // approved under a name that does not collide.
        org_approve(&state, hostile, vec![], Some("mallory".into()))
            .await
            .expect("an explicit, non-colliding user_id is accepted");

        // The revoke grammar is now unambiguous: alice's laptop is alice's laptop.
        let out = org_revoke(&state, "alice/laptop".into(), false)
            .await
            .unwrap();
        assert_eq!(out.mode, "device");
        let mesh = state.mesh_required().unwrap();
        let members = crate::daemon::roster_members(mesh);
        assert!(
            members.users.iter().any(|u| u.user_id == "mallory"),
            "revoking alice's laptop must not have touched the other member"
        );
    }

    /// #66: `user_key` rotates a PERSON's key, so a device-shaped target is contradictory.
    ///
    /// Refused by name rather than falling through to `no such person 'alice/laptop'`, which reads
    /// like the person is missing when the real problem is the caller combined two grammars.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rotation_refuses_a_device_shaped_target() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;
        org_create(&state, "acme".into(), None, None).await.unwrap();
        let alice = SigningKey::from_bytes(&[9u8; 32]);
        org_approve(
            &state,
            join_code_signed_by(&alice, &alice.verifying_key().to_bytes(), &[42u8; 32]),
            vec![],
            None,
        )
        .await
        .unwrap();

        let err = org_revoke(&state, "alice/laptop".into(), true)
            .await
            .expect_err("a rotation with a device target must be refused");
        assert!(
            format!("{err:#}").contains("must be a user_id"),
            "the refusal must name the grammar mistake: {err:#}"
        );
    }

    /// #66: an embedder can INSPECT a join code before approving it — the read that makes an
    /// "approve this person" button correct rather than merely possible.
    ///
    /// The fingerprint has to be confirmed out-of-band BEFORE the approval; a substituted code is
    /// caught there or not at all. Reading it off `org_approve`'s result is too late — the member is
    /// already in the signed roster. The CLI always had this (it decodes locally); an embedder did
    /// not, because the join-code format lives in this crate and not on the control seam.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_join_code_can_be_inspected_before_it_is_approved() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;
        org_create(&state, "acme".into(), None, None).await.unwrap();

        let alice = SigningKey::from_bytes(&[9u8; 32]);
        let code = join_code_signed_by(&alice, &alice.verifying_key().to_bytes(), &[42u8; 32]);

        let seen = org_join_code(&state, code.clone())
            .await
            .expect("a well-formed code inspects");
        assert_eq!(seen.display_name, "Alice");
        assert_eq!(seen.requested_user_id, "alice");
        assert_eq!(seen.device_label, "laptop");
        assert!(!seen.join_code_fingerprint.is_empty());

        // Read-only: nothing was installed, so the roster is untouched and the member absent.
        let mesh = state.mesh_required().unwrap();
        assert_eq!(
            mesh.roster.view().unwrap().serial(),
            1,
            "inspection must not bump the serial"
        );
        assert!(crate::daemon::roster_members(mesh).users.is_empty());

        // THE property: the fingerprint the operator confirms is the one the approval acts on.
        // Two implementations could drift, and then the code confirmed is not the code approved.
        let approved = org_approve(&state, code, vec![], None).await.unwrap();
        assert_eq!(
            approved.join_code_fingerprint, seen.join_code_fingerprint,
            "the inspected and approved fingerprints must be identical — otherwise the operator's \
             out-of-band check certifies a different code than the one that lands"
        );

        // A forged binding is REFUSED by the inspection, not described — an approve UI must not be
        // able to render a forged code as if it were a person awaiting approval.
        let mallory = SigningKey::from_bytes(&[7u8; 32]);
        let forged = join_code_signed_by(&mallory, &alice.verifying_key().to_bytes(), &[43u8; 32]);
        let err = org_join_code(&state, forged).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("device binding failed"),
            "{err:#}"
        );
    }

    /// #66: authoring verbs on a node that never ran `org_create` must refuse clearly rather than
    /// minting a root and silently forking the org.
    #[tokio::test(flavor = "multi_thread")]
    async fn authoring_on_a_non_operator_node_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;

        let err = org_revoke(&state, "alice".into(), false).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("not an org operator"),
            "revoke on a non-operator must say so: {err:#}"
        );
        // A VALID join code, so this cannot pass on the binding check instead.
        let alice = SigningKey::from_bytes(&[9u8; 32]);
        let code = join_code_signed_by(&alice, &alice.verifying_key().to_bytes(), &[42u8; 32]);
        let err = org_approve(&state, code, vec![], None).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("not an org operator"),
            "approve on a non-operator must say so once the code itself is valid: {err:#}"
        );
    }

    /// #93 ask c END TO END on the operator side: `org_rotate` publishes a roster a member still
    /// pinned to the OLD root can install, and promotes the key.
    ///
    /// The consuming half — that such a roster validates and re-anchors — is pinned in
    /// `trust::roster::validate`. This is the half that produces it, and the two meet here: the
    /// published document is fed to the REAL validator against the OLD anchor.
    #[tokio::test(flavor = "multi_thread")]
    async fn org_rotate_publishes_a_roster_the_old_anchor_can_still_install() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;
        let mesh = state.mesh().unwrap();
        org_create(&state, "acme".into(), None, None)
            .await
            .expect("org created");

        let key_path = org_root_key_path(mesh);
        let old_pk = {
            let (k, _) = OrgRootKey::load_or_generate(&key_path).unwrap();
            k.public_bytes()
        };
        let installed_before: Roster = serde_json::from_slice(
            &std::fs::read(crate::daemon::roster_install::installed_roster_path(mesh)).unwrap(),
        )
        .unwrap();

        let out = org_rotate(&state, None).await.expect("rotate succeeds");
        assert_ne!(
            out.new_root_pk,
            mcpmesh_trust::roster::encode_b64u(&old_pk),
            "the anchor actually moved"
        );
        assert_ne!(out.old_root_fingerprint, out.new_root_fingerprint);

        // The key was PROMOTED: `org-root.key` is now the successor, and the staging file is gone.
        // A rotation that left the old key in place would sign the next roster with a key members
        // have been told to stop trusting.
        let (now_key, _) = OrgRootKey::load_or_generate(&key_path).unwrap();
        assert_eq!(
            mcpmesh_trust::roster::encode_b64u(&now_key.public_bytes()),
            out.new_root_pk,
            "org-root.key must BE the successor after a rotation"
        );
        assert!(
            !key_path.with_file_name("org_root_next.key").exists(),
            "the staging key is promoted, not left behind for the next rotation to reuse"
        );

        // THE property: a member still pinned to the OLD root installs the published roster,
        // through the real validator, and is told to re-anchor.
        let published: Roster = serde_json::from_slice(
            &std::fs::read(crate::daemon::roster_install::installed_roster_path(mesh)).unwrap(),
        )
        .unwrap();
        assert!(
            published.serial > installed_before.serial,
            "rotation bumps the serial, so members converge on it"
        );
        let old_vk = mcpmesh_trust::ed25519_dalek::VerifyingKey::from_bytes(&old_pk).unwrap();
        let (_view, anchor) = mcpmesh_trust::roster::validate::validate_for_install_with_anchor(
            &published,
            &old_vk,
            installed_before.serial,
            crate::util::epoch_now_i64(),
        )
        .expect("a member on the OLD anchor must be able to install the rotated roster");
        assert_eq!(
            anchor.adopted_root_pk.as_deref(),
            Some(out.new_root_pk.as_str()),
            "…and must be told to re-anchor to exactly the key the operator published"
        );

        // The operator's OWN config was re-pinned, durably — otherwise its next restart trusts a
        // key that no longer signs anything.
        let cfg = crate::config::Config::load(&mesh.config_path).unwrap();
        assert_eq!(
            cfg.identity.org_root_pk.as_deref(),
            Some(out.new_root_pk.as_str()),
            "the operator node re-pins too"
        );
    }

    /// Rotating to the SAME key is refused rather than published.
    ///
    /// It would tell every member to re-anchor to the key they already have — a no-op that still
    /// burns a serial and, worse, reads in the audit log as a rotation that happened.
    #[tokio::test(flavor = "multi_thread")]
    async fn rotating_to_the_same_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;
        let mesh = state.mesh().unwrap();
        org_create(&state, "acme".into(), None, None)
            .await
            .expect("org created");
        let key_path = org_root_key_path(mesh);
        let e = org_rotate(&state, Some(key_path.display().to_string()))
            .await
            .expect_err("rotating to the current key must be refused");
        assert!(
            format!("{e:#}").contains("same as the current root"),
            "the refusal must say why: {e:#}"
        );
    }

    /// #93 ask c — THE MEMBER SIDE: a node pinned to the OLD root installs a rotated roster and
    /// re-anchors DURABLY.
    ///
    /// This is the path the feature exists for, and it is NOT the one the operator takes: an
    /// operator re-pins by authoring (it passes an explicit `org_root_pk`), so a test of the
    /// operator proves nothing about adoption. Deleting `adopt_successor_root`'s body left the
    /// operator test green — this is what catches it.
    ///
    /// The member is deliberately never told the new key out of band. All it has is its old pin and
    /// the roster, which is exactly the situation of a machine that was closed when the operator
    /// rotated.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_on_the_old_anchor_re_anchors_when_it_installs_a_rotated_roster() {
        // --- operator: create + rotate, and keep the published roster.
        let op_dir = tempfile::tempdir().unwrap();
        let op = operator_state(op_dir.path()).await;
        let op_mesh = op.mesh().unwrap();
        org_create(&op, "acme".into(), None, None).await.unwrap();
        let old_pk_b64u = {
            let (k, _) = OrgRootKey::load_or_generate(&org_root_key_path(op_mesh)).unwrap();
            mcpmesh_trust::roster::encode_b64u(&k.public_bytes())
        };
        let before = std::fs::read(crate::daemon::roster_install::installed_roster_path(
            op_mesh,
        ))
        .expect("pre-rotation roster");
        let rotated = org_rotate(&op, None).await.expect("rotate");
        let after = std::fs::read(crate::daemon::roster_install::installed_roster_path(
            op_mesh,
        ))
        .expect("rotated roster");

        // --- member: a separate node, pinned to the OLD root, already holding the pre-rotation
        // roster so the serial actually advances.
        let m_dir = tempfile::tempdir().unwrap();
        let m = operator_state(m_dir.path()).await;
        let m_mesh = m.mesh().unwrap();
        let pre = m_dir.path().join("pre.json");
        std::fs::write(&pre, &before).unwrap();
        crate::daemon::roster_install::install_roster(
            &m,
            pre.display().to_string(),
            Some(old_pk_b64u.clone()),
        )
        .await
        .expect("the member installs the pre-rotation roster on its old anchor");
        assert_eq!(
            crate::config::Config::load(&m_mesh.config_path)
                .unwrap()
                .identity
                .org_root_pk
                .as_deref(),
            Some(old_pk_b64u.as_str()),
            "precondition: the member is pinned to the OLD root"
        );

        // …and now the rotated one, with NO out-of-band knowledge of the new key.
        let rot = m_dir.path().join("rotated.json");
        std::fs::write(&rot, &after).unwrap();
        crate::daemon::roster_install::install_roster(&m, rot.display().to_string(), None)
            .await
            .expect(
                "a member on the old anchor must be able to install the rotated roster — that is \
                 the entire feature",
            );

        assert_eq!(
            crate::config::Config::load(&m_mesh.config_path)
                .unwrap()
                .identity
                .org_root_pk
                .as_deref(),
            Some(rotated.new_root_pk.as_str()),
            "the member must RE-ANCHOR DURABLY — a rotation held only in memory reverts on restart \
             and re-strands the node, which is the failure this feature exists to remove"
        );
    }

    /// #93 ask c gate: an ADOPTED successor beats the caller's explicit `--org-root-pk`.
    ///
    /// The caller's pk is by construction the OLD anchor — it is what they had to supply to make
    /// the roster verify at all. The first cut pinned it AFTER adoption, silently reverting the
    /// re-anchor while the `org_root_rotated` audit record had already fired. That is exactly the
    /// "reads as durable, then silently reverts" failure the code cites #107 for, and it is
    /// reachable by the by-hand distribution the CLI itself recommends.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_adopted_successor_beats_an_explicit_old_pk() {
        let op_dir = tempfile::tempdir().unwrap();
        let op = operator_state(op_dir.path()).await;
        let op_mesh = op.mesh().unwrap();
        org_create(&op, "acme".into(), None, None).await.unwrap();
        let old_pk_b64u = {
            let (k, _) = OrgRootKey::load_or_generate(&org_root_key_path(op_mesh)).unwrap();
            mcpmesh_trust::roster::encode_b64u(&k.public_bytes())
        };
        let before = std::fs::read(crate::daemon::roster_install::installed_roster_path(
            op_mesh,
        ))
        .unwrap();
        let rotated = org_rotate(&op, None).await.expect("rotate");
        let after = std::fs::read(crate::daemon::roster_install::installed_roster_path(
            op_mesh,
        ))
        .unwrap();

        let m_dir = tempfile::tempdir().unwrap();
        let m = operator_state(m_dir.path()).await;
        let m_mesh = m.mesh().unwrap();
        let pre = m_dir.path().join("pre.json");
        std::fs::write(&pre, &before).unwrap();
        crate::daemon::roster_install::install_roster(
            &m,
            pre.display().to_string(),
            Some(old_pk_b64u.clone()),
        )
        .await
        .unwrap();

        // The member is handed the rotated roster WITH the old pk again — the natural shape when
        // distributing by hand, or for a joiner holding only the org_create invite.
        let rot = m_dir.path().join("rotated.json");
        std::fs::write(&rot, &after).unwrap();
        crate::daemon::roster_install::install_roster(
            &m,
            rot.display().to_string(),
            Some(old_pk_b64u.clone()),
        )
        .await
        .expect("installs on the strength of the bridge");

        assert_eq!(
            crate::config::Config::load(&m_mesh.config_path)
                .unwrap()
                .identity
                .org_root_pk
                .as_deref(),
            Some(rotated.new_root_pk.as_str()),
            "the ADOPTED successor must win over the explicit old pk — pinning the caller's value \
             back reverts the rotation while the audit record says it happened"
        );
    }

    /// A rotated roster declares the ROTATION FORMAT, and the two must agree.
    ///
    /// The schema's own rule is that additive fields on this security document are a format bump,
    /// never a silent `#[serde(default)]`. The first cut broke that rule, and the gate proved the
    /// cost: `deny_unknown_fields` means a rotated roster does not PARSE on a pre-0.47.0 member, so
    /// — because the bridge rides every later roster — that member is partitioned permanently, with
    /// "unknown field" as its only clue. Declaring `/2` cannot make an old binary verify a key it
    /// has never seen; it makes the refusal legible.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rotated_roster_declares_the_rotation_format() {
        let dir = tempfile::tempdir().unwrap();
        let state = operator_state(dir.path()).await;
        let mesh = state.mesh().unwrap();
        org_create(&state, "acme".into(), None, None).await.unwrap();

        let path = crate::daemon::roster_install::installed_roster_path(mesh);
        let before: Roster = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            before.format,
            mcpmesh_trust::roster::ROSTER_FORMAT,
            "an org that never rotated keeps declaring /1, byte-identically"
        );
        assert!(before.successor_root_pk.is_none());

        org_rotate(&state, None).await.expect("rotate");
        let after: Roster = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            after.format,
            mcpmesh_trust::roster::ROSTER_FORMAT_ROTATION,
            "a rotation MUST bump the format, or a pre-0.47.0 member sees `unknown field` and has \
             no way to tell a newer format from a corrupt document"
        );
        assert!(after.successor_root_pk.is_some() && after.successor_sig.is_some());
    }
}
