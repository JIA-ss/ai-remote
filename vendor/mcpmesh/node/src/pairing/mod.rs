//! Pairing invites. An invite is a bearer credential the inviter mints and hands out-of-band; the
//! redeemer dials the inviter's addr on ALPN `mcpmesh/pair/1`, proves the secret, and both write
//! mutual [`PeerEntry`] rows.
//!
//! **Single-use by default, up to `max_uses` when asked (#87).** Each redemption is its own
//! ceremony — its own SAS, its own mutually authenticated peer rows — so a multi-use invite is N
//! independent pairings sharing one secret, never a group identity. The count is part of the
//! credential: it is persisted, and a redemption that cannot be recorded is refused.
//!
//! This module is pure types + logic (no iroh, no daemon): the [`Invite`] wire type + its
//! `mcpmesh-invite:` line codec, and [`LiveInvites`] — the daemon's in-RAM registry of
//! outstanding invites. The rendezvous handler mints into and redeems out of it.
//!
//! [`PeerEntry`]: crate::allowlist::PeerEntry
pub mod persist;
/// On-disk persistence for outstanding invites (#87b) — see the module doc for why a bearer
/// secret is written to disk at all, and why it is not the redb trust store.
/// The user-key RECOVERY PHRASE (#85 ask 2) — the artifact that survives the hardware.
pub mod recovery;
pub mod rendezvous;
pub mod sas;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// The `b64u:` half of a device→user binding this person's key makes for `endpoint_id` (#86).
///
/// The SAME primitive pairing presents for our OWN endpoint — `present` signs an arbitrary endpoint
/// id, which is exactly why self-enrollment needs no key transfer.
pub fn binding_sig_for(user_key: &mcpmesh_trust::UserKey, endpoint_id: &[u8; 32]) -> String {
    mcpmesh_trust::binding::present(user_key, endpoint_id).1
}

/// The scheme prefix of the single copyable pairing artifact.
const INVITE_SCHEME: &str = "mcpmesh-invite:";

/// The scheme for a SELF-ENROLLMENT invite (#86). A DIFFERENT prefix, deliberately.
///
/// `as_self` alone is not enough: it is `#[serde(default)]`, so a redeemer on a pre-43 build
/// decodes a self-invite as an ORDINARY one — writes a peer row and calls the unconditional
/// grant-back, handing the inviter access to every service it serves — while the inviter takes the
/// enrollment path and grants nothing. A silent, asymmetric, over-granting outcome across a version
/// skew, which is exactly the shape a phone app pinned to an older node hits.
///
/// A distinct scheme makes that impossible: an older daemon fails `decode` with "not an mcpmesh
/// invite", which is a clean refusal rather than a wrong ceremony.
const ENROLL_SCHEME: &str = "mcpmesh-enroll:";

/// Is this line a SELF-ENROLLMENT invite (#178)? The pre-screen an EMBEDDER needs.
///
/// [`Invite::decode`] accepts both schemes, so a `mcpmesh-enroll:` line pasted into a UI's ordinary
/// "join" field is a well-formed invite that runs a materially different ceremony. An embedder can
/// refuse it at `pair` — [`PairParams::allow_self_enroll`] defaults to `false` — but a refusal is a
/// recovery, not a prompt. This lets a UI ask the right question in the first place ("this is a
/// device-enrollment link — add this device to your account?") before it calls anything.
///
/// **The predicate is exported and the scheme constants are not**, deliberately. A `pub const`
/// invites `line.starts_with(ENROLL_SCHEME)` at the call site, which is a second copy of the
/// acceptance rule free to drift from this module's — the hand-copied-constant shape #147/#159
/// exist to remove. This function IS the rule.
///
/// Sound as a pre-screen because [`Invite::decode`] refuses any line whose scheme and `as_self`
/// disagree: for every line that decodes at all, this equals the decoded `as_self`. For a line that
/// does NOT decode it is still the right answer to prompt on — a malformed enrollment line is not
/// an ordinary invite.
///
/// [`PairParams::allow_self_enroll`]: mcpmesh_local_api::PairParams::allow_self_enroll
pub fn is_enrollment_line(line: &str) -> bool {
    line.starts_with(ENROLL_SCHEME)
}

/// Whether the CALLER offered a self-enrollment ceremony (#178).
///
/// Passed to [`rendezvous::redeem_invite`] as a REQUIRED argument rather than read off a config or
/// defaulted, so the un-offered ceremony is unrepresentable: every caller — the control seam, an
/// embedder driving the redeemer directly, a test — has to say which ceremony it is willing to
/// complete. Enforcing it only in the `pair` handler would leave `redeem_invite`'s other callers
/// exactly as exposed as #178 found them.
///
/// A named type rather than a bare `bool` because `redeem_invite` already takes eight arguments; a
/// ninth positional boolean is unreadable at the call site, which is precisely where the security
/// decision is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfEnroll {
    /// A `mcpmesh-enroll:` line is refused before any dial. The default everywhere the caller has
    /// not explicitly offered the ceremony.
    Refuse,
    /// A `mcpmesh-enroll:` line completes the enrollment: this device adopts the inviter's binding
    /// and becomes another device of that person.
    Allow,
}

/// A pairing invite. Serialized to the `mcpmesh-invite:` line, carried out-of-band, and redeemed
/// over `mcpmesh/pair/1` — once by default, up to `uses_remaining` times (#87).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// Bearer credential (32 CSPRNG bytes). Admits `uses_remaining` redemptions before it burns.
    pub secret: [u8; 32],
    /// The redeemer verifies the TLS peer id == this (the address-swap defense).
    pub inviter_id: [u8; 32],
    /// The inviter's iroh `EndpointAddr` as `serde_json` — dialable, so pairing needs NO
    /// discovery (works on localhost).
    pub inviter_addr_json: String,
    /// Suggested nickname for the inviter (the redeemer's local name for it).
    pub nickname: String,
    /// Services the redeemer is granted (may dial on the inviter).
    pub services: Vec<String>,
    /// Absolute expiry, epoch seconds; `≤ now + 24h`. The daemon enforces it.
    pub expires_at_epoch: u64,
    /// An OPAQUE, caller-chosen application label the inviter attaches at `invite` time, echoed
    /// to the redeemer in the `pair` result (#31). mcpmesh NEVER interprets it: it is never a
    /// nickname, never resolved by `open_session`, never an `allow` authorization token — purely
    /// a per-pairing metadata slot (e.g. the inviter's app-level URN, a manifest hint). Additive:
    /// `#[serde(default)]` so an old invite line decodes to `None` and an old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_label: Option<String>,
    /// The INVITER's own local name for whoever redeems this invite (#87), overriding the nickname
    /// the redeemer claims for itself.
    ///
    /// **Never rides the line.** [`encode`](Self::encode) strips it: this is what *we* call *them*,
    /// the redeemer has no business knowing it, and an invite line is a copyable artifact that gets
    /// pasted into chats. It is persisted (so it survives a restart alongside the invite) and read
    /// at redemption time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_nickname: Option<String>,
    /// SELF-ENROLLMENT (#86): the redeemer becomes another DEVICE of the inviter's person, not a
    /// peer. Rides the line — the redeemer must know to adopt a binding rather than pair.
    ///
    /// `#[serde(default)]` so an invite minted by an older daemon decodes as an ordinary one; the
    /// dangerous direction (an old daemon treating a self-invite as ordinary) is impossible because
    /// an old daemon cannot have minted one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub as_self: bool,
    /// Redemptions still available on this invite (#87). `1` for an ordinary single-use invite,
    /// which is what an absent field means — so an invite line minted by an older daemon decodes
    /// as single-use rather than as unusable.
    ///
    /// Decremented per redemption; the invite is BURNED when it reaches zero, so the terminal
    /// state is byte-identical to the single-use behaviour and the accept gate's `count() == 0`
    /// check needs no change.
    #[serde(default = "mcpmesh_local_api::one_use")]
    pub uses_remaining: u32,
}

/// The maximum length of [`Invite::app_label`], in bytes. The invite line is a human-copied
/// base32 artifact, so the opaque label is kept modest; the daemon rejects a longer one at mint.
pub const MAX_APP_LABEL_LEN: usize = 256;

impl Invite {
    /// One `mcpmesh-invite:<payload>` line. Payload = base32(no-pad) of the JSON-serialized
    /// invite (opaque to humans; the only artifact copied out-of-band — surface
    /// #2). Base32-nopad keeps the line to `[A-Z2-7]` — copy/paste-safe, case-forgiving,
    /// no `=` padding.
    pub fn encode(&self) -> String {
        // #87: `peer_nickname` is the inviter's PRIVATE local name for the redeemer. Stripped here
        // rather than never stored, because it must survive a restart with the invite — but it must
        // not travel. A `#[serde(skip)]` would have taken it out of persistence too.
        let wire = Self {
            peer_nickname: None,
            ..self.clone()
        };
        let json = serde_json::to_vec(&wire).expect("invite serializes");
        format!(
            "{}{}",
            if self.as_self {
                ENROLL_SCHEME
            } else {
                INVITE_SCHEME
            },
            data_encoding::BASE32_NOPAD.encode(&json)
        )
    }

    /// Parse a `mcpmesh-invite:` line: strip the scheme, base32-decode, JSON-deserialize.
    /// Errors on a missing scheme, an undecodable payload, or JSON that is not an [`Invite`].
    pub fn decode(line: &str) -> anyhow::Result<Self> {
        let payload = line
            .strip_prefix(INVITE_SCHEME)
            .or_else(|| line.strip_prefix(ENROLL_SCHEME))
            .ok_or_else(|| {
                anyhow::anyhow!("not an mcpmesh invite (missing {INVITE_SCHEME} scheme)")
            })?;
        let json = data_encoding::BASE32_NOPAD
            .decode(payload.as_bytes())
            .context("invite payload is not valid base32")?;
        let invite: Self =
            serde_json::from_slice(&json).context("invite payload is not a valid invite")?;
        // The SCHEME and the FLAG must agree. Otherwise a hand-built line could carry
        // `as_self: true` under the ordinary scheme (so an older peer pairs while we enroll) or the
        // reverse — reintroducing exactly the version-skew hazard the separate scheme exists to
        // prevent.
        let scheme_says_self = line.starts_with(ENROLL_SCHEME);
        anyhow::ensure!(
            invite.as_self == scheme_says_self,
            "invite scheme and as_self disagree — refusing rather than guessing which ceremony \
             this line is for"
        );
        Ok(invite)
    }
}

/// The outcome of a redemption attempt against [`LiveInvites`].
///
/// A pure lookup: an unknown/wrong secret is not in the map → [`Redeem::Unknown`] with NO state
/// change; a matched-but-stale secret → [`Redeem::Expired`] (removed); a matched live secret →
/// [`Redeem::Ok`] (BURNED). There is deliberately no "wrong guess" outcome — see [`LiveInvites`]
/// for why the security model is the secret's entropy, not an attempt cap.
#[derive(Debug)]
pub enum Redeem {
    /// The secret matched a live, unexpired invite. Its use count is decremented, and it is
    /// BURNED (removed) when that reaches zero — so a single-use invite is burned on first
    /// redemption exactly as before (#87). The returned copy carries the count AFTER this
    /// redemption.
    Ok(Invite),
    /// The secret matched an invite that had already expired; it was removed.
    Expired,
    /// The secret matches no outstanding invite (no state changed).
    Unknown,
    /// The redemption could not be RECORDED, so it was refused and rolled back (#87).
    ///
    /// A use count is part of the credential: if we cannot persist that one was spent, granting it
    /// would let a restart re-issue it. With `max_uses` up to 64 that is not a bounded slip — it is
    /// up to `max_uses` extra redemptions per restart, for every restart inside the TTL.
    ///
    /// So redemption fails CLOSED, the same direction `mint` fails: a transient write problem
    /// denies a pairing that can be retried, rather than silently over-issuing a bearer credential
    /// nobody can count.
    Unavailable,
}

/// The daemon's in-RAM registry of outstanding invites, keyed by secret.
///
/// **Model.** The redeemer SENDS a secret; the daemon LOOKS IT UP. A wrong/absent secret is
/// simply not in the map → [`Redeem::Unknown`], NO state change — so probing random secrets can
/// never burn or perturb a real invite. A matched live invite has its use count decremented and is
/// BURNED when that reaches zero — once, for an ordinary single-use invite (#87). Security rests ENTIRELY on the 32-byte CSPRNG secret (2^256), NOT on any
/// attempt cap: a per-invite guess counter would be both useless here (a stranger's garbage
/// secret is unattributable to any invite → `Unknown`) AND harmful (attributing garbage to
/// invites would let a stranger invalidate every live invite), so there is none. Map growth is
/// bounded by expiry — [`remove_expired`](Self::remove_expired) is reaped before each production
/// mint (`daemon::mint_invite`). Stranger-flood hardening of the by-design-open pair ALPN (rate
/// limit / read timeout / accept-gate) lives in the accept loop, not in a per-invite cap.
///
/// **Durable since #87b.** The registry was RAM-only, so every restart dropped every outstanding
/// invite while the invite advertised a 24h TTL — an invite emailed to a colleague was reliably
/// dead within a couple of hours on a node that auto-updates. With a [`persist::InviteFile`]
/// attached, every mutation is written through, and [`load`](Self::load) restores the live set at
/// boot. Without one (tests, a control-only node) it behaves exactly as before.
#[derive(Default)]
pub struct LiveInvites {
    inner: Mutex<HashMap<[u8; 32], Invite>>,
    /// Where mutations are written through, if anywhere. `None` = RAM-only (the old behaviour),
    /// which is what every test that does not care about durability gets.
    file: Option<persist::InviteFile>,
    /// Serializes WRITERS, and is the reason the mutating methods are `async`.
    ///
    /// The mutation and its write both happen under this, so two concurrent mints cannot land
    /// their snapshots out of order and lose one. It is a tokio mutex because it is held across
    /// the `spawn_blocking` that does the actual fsync — the `inner` std mutex is still only ever
    /// held for the map operation itself, never across an await.
    writes: tokio::sync::Mutex<()>,
}

impl LiveInvites {
    /// A fresh, empty RAM-only registry — equivalent to [`Default::default`], provided so daemon
    /// call sites read `LiveInvites::new()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry backed by `path`, restored from whatever is still live at `now_epoch`.
    ///
    /// Expired invites are dropped AND the file is rewritten without them, so it cannot accumulate
    /// dead entries across restarts. A rewrite failure here is not fatal — the live set is already
    /// correct in memory, and the next mint will surface a persistent write problem as an error
    /// where it actually matters.
    pub fn load(path: impl Into<std::path::PathBuf>, now_epoch: u64) -> Self {
        let file = persist::InviteFile::new(path);
        // `load` applies the expiry filter, so compare against what is ACTUALLY on disk to decide
        // whether a rewrite is owed.
        //
        // The first version used `!live.is_empty()`, which reads as "something was reaped" and
        // means "something survived" — inverted in both directions (#87b gate). It rewrote on
        // every boot when nothing had expired, and — the one that matters — rewrote NOTHING when
        // everything had. A node whose invites had all expired then held their secrets at rest
        // indefinitely, because with zero live invites no later mutation ever rewrites the file.
        let on_disk = file.load(0).len();
        let live = file.load(now_epoch);
        let reaped = live.len() != on_disk;
        let map: HashMap<[u8; 32], Invite> = live.into_iter().map(|i| (i.secret, i)).collect();
        if reaped && let Err(e) = file.store(&map.values().collect::<Vec<_>>()) {
            tracing::warn!(%e, "could not rewrite the invite file after reaping expired invites");
        }
        Self {
            inner: Mutex::new(map),
            file: Some(file),
            writes: tokio::sync::Mutex::new(()),
        }
    }

    /// Write `snapshot` through to disk, OFF the runtime worker.
    ///
    /// `fsync` is blocking, and this repo's house rule is that blocking work never runs on a
    /// runtime worker — the same rule that already sends a redb READ in the rendezvous through
    /// `spawn_blocking`, which is strictly cheaper than this. It matters more here than usual: the
    /// ALPN_PAIR accept gate takes `inner` on every inbound pair connection, so an fsync held
    /// under that lock on a slow or network-mounted data dir would park workers on a path whose
    /// job is to accept connections.
    ///
    /// Callers hold `writes` across this, so the snapshot they took and the write it produces
    /// cannot be reordered against another writer's.
    async fn persist(&self, snapshot: Vec<Invite>) -> std::io::Result<()> {
        let Some(file) = self.file.clone() else {
            return Ok(());
        };
        crate::util::blocking("join invite persist", move || {
            file.store(&snapshot.iter().collect::<Vec<_>>())
        })
        .await
        .map_err(std::io::Error::other)?
    }

    /// Lock the registry. The mutex is only ever held for the duration of a single
    /// map operation (never across `.await`), so poisoning means a prior holder panicked
    /// mid-mutation — unrecoverable; propagate it rather than risk a torn map.
    fn guard(&self) -> MutexGuard<'_, HashMap<[u8; 32], Invite>> {
        self.inner.lock().expect("LiveInvites mutex poisoned")
    }

    /// Insert an outstanding invite (keyed by its secret; a re-mint of the same secret would
    /// replace, but secrets are CSPRNG-unique in practice).
    ///
    /// **Fails if the invite cannot be persisted (#87b).** The advertised TTL is part of the
    /// invite's contract, and handing someone an invite we already know will not survive the next
    /// restart is exactly the defect this issue filed. A write failure in the data directory is
    /// also a real problem the operator needs to see — the trust store lives there too.
    ///
    /// The in-memory insert is rolled back on a write failure, so the registry never holds an
    /// invite the caller was told it does not have.
    pub async fn mint(&self, invite: Invite) -> std::io::Result<()> {
        let _w = self.writes.lock().await;
        let secret = invite.secret;
        // The std mutex is taken ONLY for the map op and the snapshot — never across the await.
        let (displaced, snapshot) = {
            let mut map = self.guard();
            let displaced = map.insert(secret, invite);
            (displaced, map.values().cloned().collect::<Vec<_>>())
        };
        match self.persist(snapshot).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let mut map = self.guard();
                match displaced {
                    Some(prev) => map.insert(secret, prev),
                    None => map.remove(&secret),
                };
                Err(e)
            }
        }
    }

    /// Is `secret` a LIVE invite at `now_epoch`, without redeeming it? NON-MUTATING — never
    /// burns, never reaps. The #87 collision pre-check runs behind this so a nickname collision
    /// can be refused BEFORE [`try_redeem`](Self::try_redeem) spends the secret: the check is
    /// only reached by a caller holding a live secret, so answering it is not an oracle, and
    /// the invite survives for a retry under a different name. Advisory only — `try_redeem`
    /// stays the authoritative (and racing-safe) redemption.
    pub fn peek_live(&self, secret: &[u8; 32], now_epoch: u64) -> bool {
        self.peek_live_alias(secret, now_epoch).is_some()
    }

    /// As [`peek_live`](Self::peek_live), but also yields the invite's `peer_nickname` (#87) —
    /// `Some(None)` for a live invite carrying no alias, `None` when not live.
    ///
    /// The collision PRE-CHECK needs the alias, because the name that will actually be stored is
    /// the alias when one is set. Checking the redeemer's self-claim there instead would refuse a
    /// pairing over a name we were never going to use, and let one through over the name we were.
    /// Is the live invite for `secret` a SELF-ENROLLMENT (#86)? `false` when not live.
    pub fn peek_is_self(&self, secret: &[u8; 32], now_epoch: u64) -> bool {
        self.guard()
            .get(secret)
            .is_some_and(|inv| inv.expires_at_epoch >= now_epoch && inv.as_self)
    }

    pub fn peek_live_alias(&self, secret: &[u8; 32], now_epoch: u64) -> Option<Option<String>> {
        self.guard()
            .get(secret)
            .filter(|inv| inv.expires_at_epoch >= now_epoch)
            .map(|inv| inv.peer_nickname.clone())
    }

    /// Redeem `secret` at `now_epoch`. Unknown secret → [`Redeem::Unknown`] (no state change).
    /// Known but expired → [`Redeem::Expired`] (removed). Known + live → SUCCESS: the use count is
    /// decremented, the invite is BURNED when it hits zero, and the copy returned in
    /// [`Redeem::Ok`] carries the count AFTER this redemption (#87). A redemption that cannot be
    /// RECORDED is refused as [`Redeem::Unavailable`] and rolled back.
    pub async fn try_redeem(&self, secret: &[u8; 32], now_epoch: u64) -> Redeem {
        let _w = self.writes.lock().await;
        // Decide and mutate under the std mutex, then release it BEFORE the write. `Unknown`
        // changes nothing, so it never touches the disk — probing random secrets stays free.
        let (outcome, snapshot) = {
            let mut map = self.guard();
            match map.get(secret) {
                None => return Redeem::Unknown,
                Some(inv) if inv.expires_at_epoch < now_epoch => {
                    map.remove(secret);
                    (Redeem::Expired, map.values().cloned().collect::<Vec<_>>())
                }
                Some(_) => {
                    // #87: decrement, and burn only at zero. A multi-use invite is N independent
                    // pairings sharing one secret — the collision pre-check, the post-redeem race
                    // guard, expiry and the SAS all run again per redemption. Burning at zero
                    // keeps the terminal REGISTRY state identical to single-use.
                    let entry = map.get_mut(secret).expect("present under lock");
                    // `checked_sub`, not `saturating_sub`: a zero-count entry must be refused, not
                    // granted one more redemption. Unreachable through the `invite` verb (which
                    // rejects `max_uses: 0`), but reachable by an embedder building an `Invite`
                    // directly or by a hand-edited/rolled-back invites.json — and an invariant on
                    // a bearer credential should fail closed wherever it is violated (#87 gate).
                    let Some(left) = entry.uses_remaining.checked_sub(1) else {
                        return Redeem::Unknown;
                    };
                    entry.uses_remaining = left;
                    let inv = if left == 0 {
                        map.remove(secret).expect("present under lock")
                    } else {
                        entry.clone()
                    };
                    (Redeem::Ok(inv), map.values().cloned().collect::<Vec<_>>())
                }
            }
        };
        // Fail CLOSED (#87 gate). Warning here was defensible while an invite was single-use —
        // the worst case was one extra redemption, bounded by the TTL. With a counted credential
        // it is up to `max_uses` extra redemptions per restart, and the "still refused by the
        // collision/expiry checks" consolation is false: a resurrected use admits a genuinely new
        // peer that nothing refuses.
        //
        // This runs BEFORE the caller writes any peer rows, so refusing here denies a pairing that
        // has not happened yet rather than undoing one that has.
        if let Err(e) = self.persist(snapshot).await {
            tracing::warn!(%e, "could not record an invite redemption; refusing it rather than \
                                risking a restart re-issuing the use");
            let mut map = self.guard();
            // Put it back with the count it had BEFORE this redemption. An expiry reap that
            // could not be written is deliberately NOT restored: the entry was already dead, and
            // resurrecting an expired invite would be worse than losing the reap.
            if let Redeem::Ok(inv) = &outcome {
                let mut restored = inv.clone();
                restored.uses_remaining = restored.uses_remaining.saturating_add(1);
                map.insert(*secret, restored);
            }
            return Redeem::Unavailable;
        }
        outcome
    }

    /// Number of outstanding invites — the live-invite ACCEPT-GATE check: the pair
    /// rendezvous is only "open" while an invite is live. The daemon's
    /// `spawn_accept_loop` `ALPN_PAIR` branch calls this BEFORE `handle_inviter_side`: `count() == 0`
    /// → the pair dial is closed immediately (no bi-stream, no hello, no handler task). Advisory /
    /// coarse (any-invite-live): a racing burn of the last invite is caught authoritatively by
    /// [`try_redeem`](Self::try_redeem) returning `Unknown`, so this is a cheap front-door close
    /// realizing the windowed listener over a permanently-advertised ALPN, not the security boundary.
    pub fn count(&self) -> usize {
        self.guard().len()
    }

    /// Drop every invite that has expired as of `now_epoch`. Reaped before each production mint
    /// ([`daemon::mint_invite`](crate::daemon)) so a long-lived daemon's registry cannot grow
    /// unboundedly with never-redeemed invites.
    /// Persists the reap (#87b gate). The first version mutated memory only, so a reaped invite's
    /// secret stayed on disk until some LATER mutation happened to rewrite the file — and if the
    /// mint that follows the reap failed, nothing ever did. Silent memory/disk divergence on a
    /// file of bearer credentials is not a state to leave reachable.
    pub async fn remove_expired(&self, now_epoch: u64) {
        let _w = self.writes.lock().await;
        let (changed, snapshot) = {
            let mut map = self.guard();
            let before = map.len();
            map.retain(|_, inv| inv.expires_at_epoch >= now_epoch);
            (
                map.len() != before,
                map.values().cloned().collect::<Vec<_>>(),
            )
        };
        if changed {
            inv_persist_burn(self, snapshot).await;
        }
    }
}

/// Persist an expiry REAP — a removal of invites that are already dead.
///
/// Warns rather than failing, and that is safe HERE in a way it is not for a redemption: the worst
/// case is an expired invite lingering on disk until the next mutation, and expiry is re-checked on
/// load and on every redemption, so it cannot be used. Redemption itself fails CLOSED (see
/// [`Redeem::Unavailable`]) — an unrecorded USE is a credential we have lost count of, which is a
/// different thing entirely (#87 gate).
async fn inv_persist_burn(reg: &LiveInvites, snapshot: Vec<Invite>) {
    if let Err(e) = reg.persist(snapshot).await {
        tracing::warn!(
            %e,
            "could not persist an invite burn; it may reappear after a restart until it expires"
        );
    }
}

#[cfg(test)]
mod tests {
    /// #87b: an invite outlives the registry that minted it. This is the whole issue — the
    /// registry was RAM-only while the invite line advertised a 24h TTL, so a node that
    /// auto-updates every couple of hours voided invites its users had already mailed out.
    #[tokio::test]
    async fn an_invite_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invites.json");
        let inv = sample_invite(1, 9_000);

        let first = LiveInvites::load(&path, 1_000);
        first.mint(inv.clone()).await.unwrap();
        drop(first); // the daemon exits — an update, a crash, a reboot

        let after = LiveInvites::load(&path, 2_000);
        assert_eq!(after.count(), 1, "the invite must still be outstanding");
        assert!(
            matches!(after.try_redeem(&inv.secret, 2_000).await, Redeem::Ok(_)),
            "and must still be redeemable — an invite that survives but cannot be spent is no \
             better than one that did not survive"
        );
    }

    /// #87b: a REDEMPTION is durable too. A burn that lived only in RAM would let a restart
    /// resurrect a spent single-use bearer credential.
    #[tokio::test]
    async fn a_redemption_is_not_undone_by_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invites.json");
        let inv = sample_invite(2, 9_000);

        let first = LiveInvites::load(&path, 1_000);
        first.mint(inv.clone()).await.unwrap();
        assert!(matches!(
            first.try_redeem(&inv.secret, 1_000).await,
            Redeem::Ok(_)
        ));
        drop(first);

        let after = LiveInvites::load(&path, 1_000);
        assert_eq!(after.count(), 0);
        assert!(
            matches!(after.try_redeem(&inv.secret, 1_000).await, Redeem::Unknown),
            "a spent single-use credential must not be resurrected by a restart"
        );
    }

    /// #87b: an EXPIRED invite does not come back, and the file does not accumulate.
    #[tokio::test]
    async fn an_expired_invite_is_dropped_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invites.json");
        let dead = sample_invite(3, 5_000);
        let live = sample_invite(4, 9_000);

        let first = LiveInvites::load(&path, 1_000);
        first.mint(dead.clone()).await.unwrap();
        first.mint(live.clone()).await.unwrap();
        drop(first);

        let after = LiveInvites::load(&path, 6_000); // dead has expired
        assert_eq!(after.count(), 1, "only the live one survives");
        assert!(matches!(
            after.try_redeem(&dead.secret, 6_000).await,
            Redeem::Unknown
        ));
        assert!(matches!(
            after.try_redeem(&live.secret, 6_000).await,
            Redeem::Ok(_)
        ));
    }

    /// #87b: a mint that cannot be persisted FAILS, and leaves no phantom in memory.
    ///
    /// Returning `Ok` here would hand back an invite carrying a 24h TTL that we already know will
    /// not survive the next restart — the exact defect this issue filed, re-created one layer
    /// down. The rollback matters just as much: a registry holding an invite the caller was told
    /// it does not have would accept a redemption nobody believes exists.
    #[tokio::test]
    async fn a_mint_that_cannot_persist_fails_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        // The invite path is a DIRECTORY, so the atomic rename can never succeed.
        let path = dir.path().join("invites.json");
        std::fs::create_dir(&path).unwrap();

        let reg = LiveInvites::load(&path, 1_000);
        let inv = sample_invite(5, 9_000);
        assert!(
            reg.mint(inv.clone()).await.is_err(),
            "a mint that cannot promise the TTL it advertises must not report success"
        );
        assert_eq!(reg.count(), 0, "and must leave no phantom behind");
        assert!(matches!(
            reg.try_redeem(&inv.secret, 1_000).await,
            Redeem::Unknown
        ));
    }

    /// #87b gate: a file where EVERYTHING has expired is rewritten empty.
    ///
    /// The first `load` decided "did I reap?" with `!live.is_empty()`, which actually means "did
    /// anything survive" — inverted in both directions. The consequence that matters: with zero
    /// live invites the file was never rewritten, and since no later mutation can happen on a
    /// registry with nothing in it, the expired invites' SECRETS stayed on disk indefinitely.
    ///
    /// The earlier test masked this by keeping one live invite in the fixture, which is the
    /// "empty fixture defaults hide the mutation" shape: seed only what the code exists to remove.
    #[tokio::test]
    async fn an_all_expired_file_is_rewritten_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invites.json");

        let first = LiveInvites::load(&path, 1_000);
        first.mint(sample_invite(7, 5_000)).await.unwrap();
        first.mint(sample_invite(8, 5_000)).await.unwrap();
        drop(first);

        let after = LiveInvites::load(&path, 9_000); // both expired
        assert_eq!(after.count(), 0);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, "[]",
            "expired bearer secrets must not sit at rest — with nothing live, no later mutation \
             will ever rewrite this file: {on_disk}"
        );
    }

    /// #87b gate: reaping expired invites is PERSISTED.
    ///
    /// `remove_expired` mutated memory only, so a reap survived on disk until some later mutation
    /// happened to rewrite the file — and if the mint that follows it failed, nothing ever did.
    /// Silent memory/disk divergence on a file of bearer credentials is not a reachable state
    /// worth leaving.
    #[tokio::test]
    async fn reaping_expired_invites_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invites.json");
        let reg = LiveInvites::load(&path, 1_000);
        reg.mint(sample_invite(9, 5_000)).await.unwrap();
        reg.mint(sample_invite(10, 9_000)).await.unwrap();

        reg.remove_expired(6_000).await;
        assert_eq!(reg.count(), 1, "memory drops the expired one");

        let on_disk = persist::InviteFile::new(&path).load(0);
        assert_eq!(
            on_disk.len(),
            1,
            "and so must DISK — otherwise the reaped secret is still readable: {on_disk:?}"
        );
        assert_eq!(on_disk[0].expires_at_epoch, 9_000);
    }

    /// #87: a multi-use invite admits exactly `max_uses` redemptions and then behaves like any
    /// spent invite.
    ///
    /// The fourth attempt answers `Unknown` — the SAME answer a never-existed secret gets — so
    /// exhausting an invite introduces no new oracle: a prober still cannot distinguish "spent"
    /// from "never real".
    #[tokio::test]
    async fn a_multi_use_invite_admits_exactly_its_quota() {
        let reg = LiveInvites::new();
        let mut inv = sample_invite(11, 9_000);
        inv.uses_remaining = 3;
        reg.mint(inv.clone()).await.unwrap();

        for expected_left in [2, 1, 0] {
            match reg.try_redeem(&inv.secret, 1_000).await {
                Redeem::Ok(got) => assert_eq!(
                    got.uses_remaining, expected_left,
                    "each redemption decrements, and the caller sees what is left"
                ),
                other => panic!("redemption within quota must succeed, got {other:?}"),
            }
        }
        assert_eq!(
            reg.count(),
            0,
            "at zero the invite is BURNED, exactly as single-use"
        );
        assert!(
            matches!(reg.try_redeem(&inv.secret, 1_000).await, Redeem::Unknown),
            "an exhausted invite answers Unknown — the same answer a secret that never existed \
             gets, so exhausting one is not an oracle"
        );
    }

    /// #87: the default is unchanged. An invite with no `max_uses` burns on first redemption,
    /// which is what every caller that predates this already relies on.
    #[tokio::test]
    async fn a_single_use_invite_still_burns_on_first_redemption() {
        let reg = LiveInvites::new();
        let inv = sample_invite(12, 9_000);
        assert_eq!(inv.uses_remaining, 1, "the sample IS the default shape");
        reg.mint(inv.clone()).await.unwrap();
        assert!(matches!(
            reg.try_redeem(&inv.secret, 1_000).await,
            Redeem::Ok(_)
        ));
        assert_eq!(reg.count(), 0);
    }

    /// #87 + #87b: the remaining count is DURABLE. A restart that reset it would silently hand
    /// out more redemptions than the minter authorized — the count is part of the credential.
    #[tokio::test]
    async fn the_remaining_use_count_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invites.json");
        let mut inv = sample_invite(13, 9_000);
        inv.uses_remaining = 3;

        let first = LiveInvites::load(&path, 1_000);
        first.mint(inv.clone()).await.unwrap();
        assert!(matches!(
            first.try_redeem(&inv.secret, 1_000).await,
            Redeem::Ok(_)
        ));
        drop(first);

        let after = LiveInvites::load(&path, 1_000);
        match after.try_redeem(&inv.secret, 1_000).await {
            Redeem::Ok(got) => assert_eq!(
                got.uses_remaining, 1,
                "the restart must not restore spent uses — 3 minted, 1 spent, 2 left, so this \
                 redemption leaves 1"
            ),
            other => panic!("expected a redemption, got {other:?}"),
        }
    }

    /// #87: an invite line from a daemon that predates multi-use decodes as SINGLE-use.
    ///
    /// A `default` of 0 would make every old invite instantly unusable — the field's default is
    /// load-bearing for compatibility, not a formality.
    #[test]
    fn an_invite_line_without_a_use_count_decodes_as_single_use() {
        let mut v = serde_json::to_value(sample_invite(14, 9_000)).unwrap();
        v.as_object_mut().unwrap().remove("uses_remaining");
        let old: Invite = serde_json::from_value(v).expect("an older invite must still decode");
        assert_eq!(
            old.uses_remaining, 1,
            "an invite minted before #87 is single-use, not unusable"
        );
    }

    /// #87: expiry still terminates a multi-use invite that has uses left. The two bounds are
    /// independent, and the TTL is the one that caps a leaked line's blast radius over time.
    #[tokio::test]
    async fn expiry_beats_remaining_uses() {
        let reg = LiveInvites::new();
        let mut inv = sample_invite(15, 5_000);
        inv.uses_remaining = 10;
        reg.mint(inv.clone()).await.unwrap();
        assert!(
            matches!(reg.try_redeem(&inv.secret, 6_000).await, Redeem::Expired),
            "uses left does not outlive the TTL"
        );
        assert_eq!(reg.count(), 0, "and it is removed");
    }

    /// A RAM-only registry still behaves exactly as it did before #87b — the seam is opt-in, and
    /// every test that does not care about durability keeps working unchanged.
    #[tokio::test]
    async fn a_ram_only_registry_is_unchanged() {
        let reg = LiveInvites::new();
        let inv = sample_invite(6, 9_000);
        reg.mint(inv.clone())
            .await
            .expect("a RAM-only mint cannot fail");
        assert_eq!(reg.count(), 1);
        assert!(matches!(
            reg.try_redeem(&inv.secret, 1_000).await,
            Redeem::Ok(_)
        ));
    }

    use super::*;

    fn sample_invite(secret: u8, expires_at_epoch: u64) -> Invite {
        Invite {
            secret: [secret; 32],
            inviter_id: [3u8; 32],
            inviter_addr_json: "{\"id\":\"abc\",\"addrs\":[]}".into(),
            nickname: "alice".into(),
            services: vec!["notes".into()],
            expires_at_epoch,
            app_label: None,
            uses_remaining: 1,
            peer_nickname: None,
            as_self: false,
        }
    }

    /// #178: the embedder's pre-screen must agree with what `decode` actually does.
    ///
    /// The predicate exists so a UI can PROMPT ("this is a device-enrollment link") instead of
    /// recovering from a refusal. That is only safe if it never disagrees with the ceremony that
    /// would actually run — a predicate that said "ordinary" for a line `decode` reads as an
    /// enrollment would put the wrong question in front of the person, which is the exact failure
    /// #178 is about, moved one layer up.
    ///
    /// So this asserts agreement rather than a string prefix: it compares the predicate against
    /// the DECODED `as_self` for both schemes. Hardcoding `is_enrollment_line` to `false` (or to
    /// `true`) fails one half; wiring it to `INVITE_SCHEME` fails both.
    #[test]
    fn the_enrollment_pre_screen_agrees_with_the_decoder() {
        let plain = sample_invite(7, 1_800_000_000);
        let mut enroll = sample_invite(8, 1_800_000_000);
        enroll.as_self = true;

        for inv in [&plain, &enroll] {
            let line = inv.encode();
            assert_eq!(
                is_enrollment_line(&line),
                Invite::decode(&line).unwrap().as_self,
                "the pre-screen must answer what the ceremony will actually do: {line}"
            );
        }
        // Stated explicitly so the loop above cannot pass by agreeing on one value twice.
        assert!(!is_enrollment_line(&plain.encode()));
        assert!(is_enrollment_line(&enroll.encode()));

        // A line that does not decode at all is still screened as what it CLAIMS to be — a UI
        // prompting on a malformed enrollment link must not be told it is an ordinary invite.
        assert!(is_enrollment_line("mcpmesh-enroll:!!!not-base32"));
        assert!(!is_enrollment_line("mcpmesh-invite:!!!not-base32"));
        assert!(!is_enrollment_line("https://example.com/enroll"));
        assert!(!is_enrollment_line(""));
    }

    #[test]
    fn invite_roundtrips_through_the_line_encoding() {
        let inv = sample_invite(7, 1_800_000_000);
        let line = inv.encode(); // "mcpmesh-invite:<base32 payload>"
        assert!(line.starts_with("mcpmesh-invite:"));
        let back = Invite::decode(&line).unwrap();
        assert_eq!(back, inv);
        // A payload that is not valid base32 for the scheme is rejected.
        assert!(Invite::decode("mcpmesh-invite:!!!not-valid").is_err());
        // A line missing the scheme is rejected.
        assert!(Invite::decode("notaninvite").is_err());
    }

    #[test]
    fn invite_carries_an_opaque_app_label_additively() {
        // #31: an inviter-attached opaque label round-trips through the invite line.
        let mut inv = sample_invite(9, 1_800_000_000);
        inv.app_label = Some("urn:kb-mesh:node:abc123".into());
        let back = Invite::decode(&inv.encode()).unwrap();
        assert_eq!(back.app_label.as_deref(), Some("urn:kb-mesh:node:abc123"));
        assert_eq!(back, inv);

        // An OLD invite line (JSON without the field) decodes to None — additive both ways.
        let no_label = sample_invite(9, 1_800_000_000);
        assert!(no_label.app_label.is_none());
        let json = serde_json::to_vec(&no_label).unwrap();
        // The serialized form omits the field entirely (skip_serializing_if), so it reads like a
        // pre-#31 invite; it still decodes.
        assert!(!String::from_utf8_lossy(&json).contains("app_label"));
        let line = format!(
            "mcpmesh-invite:{}",
            data_encoding::BASE32_NOPAD.encode(&json)
        );
        assert_eq!(Invite::decode(&line).unwrap().app_label, None);
    }

    #[test]
    fn decode_rejects_hostile_payloads_without_panicking() {
        // decode() is the bearer-credential parse path — hostile input must Err, never panic.
        // Empty payload after the scheme → Err (not a panic).
        assert!(Invite::decode("mcpmesh-invite:").is_err());
        // Valid base32 that decodes to well-formed-but-wrong JSON → Err (exercises the
        // serde_json::from_slice error branch, distinct from the base32 branch).
        let not_invite = data_encoding::BASE32_NOPAD.encode(b"{\"nope\":1}");
        assert!(Invite::decode(&format!("mcpmesh-invite:{not_invite}")).is_err());
    }

    #[tokio::test]
    async fn mint_then_redeem_valid_burns_the_invite() {
        let live = LiveInvites::default();
        let inv = sample_invite(7, 1_800_000_000);
        let secret = inv.secret;
        live.mint(inv.clone()).await.unwrap();
        assert_eq!(live.count(), 1);
        // First redeem succeeds and returns the invite. Since #87 the returned copy carries the
        // count AFTER this redemption — 0 for a single-use invite — so it reports what the
        // redemption left rather than what was minted. Everything else is the invite verbatim.
        match live.try_redeem(&secret, 1_000_000_000).await {
            Redeem::Ok(got) => {
                assert_eq!(got.uses_remaining, 0, "spent");
                assert_eq!(
                    Invite {
                        uses_remaining: inv.uses_remaining,
                        ..got
                    },
                    inv
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        // The invite is burned: a second redeem of the same secret is now Unknown.
        assert!(matches!(
            live.try_redeem(&secret, 1_000_000_000).await,
            Redeem::Unknown
        ));
        assert_eq!(live.count(), 0);
    }

    #[tokio::test]
    async fn redeem_unknown_secret_is_unknown_and_leaves_other_invites_untouched() {
        let live = LiveInvites::default();
        let inv = sample_invite(7, 1_800_000_000);
        live.mint(inv).await.unwrap();
        // An unknown/wrong secret consumes NOTHING — no invite's state changes.
        assert!(matches!(
            live.try_redeem(&[9u8; 32], 1_000_000_000).await,
            Redeem::Unknown
        ));
        assert_eq!(
            live.count(),
            1,
            "unknown secret must not burn a live invite"
        );
    }

    #[tokio::test]
    async fn redeem_expired_secret_is_expired_and_removed() {
        let live = LiveInvites::default();
        let inv = sample_invite(7, 1_000);
        let secret = inv.secret;
        live.mint(inv).await.unwrap();
        // now is past expiry → Expired, and the stale invite is removed.
        assert!(matches!(
            live.try_redeem(&secret, 2_000).await,
            Redeem::Expired
        ));
        assert_eq!(live.count(), 0);
    }

    #[tokio::test]
    async fn remove_expired_drops_only_the_stale_invites() {
        let live = LiveInvites::default();
        live.mint(sample_invite(1, 1_000)).await.unwrap(); // expires early
        live.mint(sample_invite(2, 9_000)).await.unwrap(); // still live at now=2_000
        assert_eq!(live.count(), 2);
        live.remove_expired(2_000).await;
        assert_eq!(live.count(), 1);
        // The surviving one is still redeemable.
        assert!(matches!(
            live.try_redeem(&[2u8; 32], 2_000).await,
            Redeem::Ok(_)
        ));
    }
}
